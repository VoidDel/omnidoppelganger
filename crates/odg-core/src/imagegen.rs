//! Pictures painted by an image model for the user: `generate_image` asks OpenAI's or Zhipu's image API, with the key
//! the user saved for that provider's chat models, and saves what comes back in the workspace.

use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Which API a painter speaks: OpenAI's Images API, which answers with the picture itself, or Zhipu's, which answers
/// with an address to fetch it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    OpenAi,
    Zhipu,
}

/// An image model the user can choose, and the provider whose key it is used with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Painter {
    /// What the settings keep.
    pub id: &'static str,
    /// The provider whose address and key it is reached with.
    pub provider: &'static str,
    /// The model id the API is asked for.
    pub model: &'static str,
    /// What the user is shown.
    pub name: &'static str,
    pub api: Api,
}

/// The painters there are, in the order one is chosen when the user has chosen none: the first whose provider has a key.
pub const PAINTERS: &[Painter] = &[
    Painter {
        id: "gpt-image-2",
        provider: "openai",
        model: "gpt-image-2",
        name: "OpenAI GPT Image 2",
        api: Api::OpenAi,
    },
    Painter {
        id: "glm-image",
        provider: "zhipu",
        model: "glm-image",
        name: "智谱 GLM-Image（高清）",
        api: Api::Zhipu,
    },
    Painter {
        id: "cogview-4",
        provider: "zhipu",
        model: "cogview-4-250304",
        name: "智谱 CogView-4",
        api: Api::Zhipu,
    },
    Painter {
        id: "cogview-3-flash",
        provider: "zhipu",
        model: "cogview-3-flash",
        name: "智谱 CogView-3 Flash（快）",
        api: Api::Zhipu,
    },
];

/// Sentinel `settings.painter` value that switches image generation off completely.
pub const PAINTER_OFF: &str = "off";

pub fn painter(id: &str) -> Option<&'static Painter> {
    PAINTERS.iter().find(|painter| painter.id == id)
}

/// Where a picture is asked for, and with which key and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageConfig {
    pub painter: &'static Painter,
    /// The API's address for painting.
    pub url: String,
    pub key: String,
}

/// Gives the painter in force, or `None` when no provider with an image model has a key.
pub type ImageSource = std::sync::Arc<dyn Fn() -> Option<ImageConfig> + Send + Sync>;

/// The shapes a picture is asked in; each painter has its own sizes for them.
pub const SHAPES: &[&str] = &["square", "landscape", "portrait"];

/// The size a painter is asked for, for a shape: sizes each API lists among those it recommends.
pub fn size(painter: &Painter, shape: &str) -> &'static str {
    match (painter.api, painter.id, shape) {
        (Api::OpenAi, _, "landscape") => "1536x1024",
        (Api::OpenAi, _, "portrait") => "1024x1536",
        (Api::OpenAi, _, _) => "1024x1024",
        (Api::Zhipu, "glm-image", "landscape") => "1568x1056",
        (Api::Zhipu, "glm-image", "portrait") => "1056x1568",
        (Api::Zhipu, "glm-image", _) => "1280x1280",
        (Api::Zhipu, _, "landscape") => "1344x768",
        (Api::Zhipu, _, "portrait") => "768x1344",
        (Api::Zhipu, _, _) => "1024x1024",
    }
}

/// A painting takes a while; the high quality ones more.
const TIMEOUT: Duration = Duration::from_secs(240);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest picture taken from an address.
const PICTURE_BYTES: usize = 40 * 1024 * 1024;

/// A picture as it came back: its bytes, and the extension its kind is saved with.
#[derive(Debug)]
pub struct Painted {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    /// The size it was asked in.
    pub size: &'static str,
}

/// Ask the painter for one picture of `prompt`, in `shape`, `high` quality or the standard one.
pub async fn paint(
    config: &ImageConfig,
    prompt: &str,
    shape: &str,
    high: bool,
    cancel: &CancellationToken,
) -> Result<Painted, String> {
    let painter = config.painter;
    let size = size(painter, shape);
    let body = match painter.api {
        Api::OpenAi => {
            json!({ "model": painter.model, "prompt": prompt, "size": size, "quality": if high { "high" } else { "medium" }, "n": 1, "output_format": "png" })
        }
        // GLM-Image paints in high definition only.
        Api::Zhipu => {
            json!({ "model": painter.model, "prompt": prompt, "size": size, "quality": if high || painter.id == "glm-image" { "hd" } else { "standard" } })
        }
    };
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| format!("Cannot set up the request: {error}"))?;
    let request = client.post(&config.url).bearer_auth(config.key.trim()).json(&body);
    let answered = async {
        let response = request
            .send()
            .await
            .map_err(|error| format!("Cannot reach {}: {error}", painter.name))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("{}'s answer broke off: {error}", painter.name))?;
        Ok::<_, String>((status, text))
    };
    let (status, text) = tokio::select! {
        outcome = answered => outcome?,
        () = cancel.cancelled() => return Err("The painting was cancelled.".to_string()),
    };
    let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let first = &answer["data"][0];
    if !status.is_success() || !first.is_object() {
        let error = if answer["error"].is_object() {
            &answer["error"]
        } else {
            &answer
        };
        let message = error["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| text.chars().take(300).collect());
        let code = error["code"]
            .as_str()
            .map(str::to_string)
            .or_else(|| error["code"].as_i64().map(|code| code.to_string()))
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(format!("{} did not paint it ({code}): {message}", painter.name));
    }
    match painter.api {
        Api::OpenAi => {
            let encoded = first["b64_json"]
                .as_str()
                .ok_or_else(|| format!("{} answered without a picture.", painter.name))?;
            let bytes = unbase64(encoded).ok_or_else(|| format!("{}'s picture could not be read.", painter.name))?;
            Ok(Painted {
                extension: kind_of(&bytes).unwrap_or("png"),
                bytes,
                size,
            })
        }
        Api::Zhipu => {
            // The address lasts a while only, so the picture is taken at once.
            let address = first["url"]
                .as_str()
                .ok_or_else(|| format!("{} answered without a picture.", painter.name))?;
            let fetched = async {
                let response = client
                    .get(address)
                    .send()
                    .await
                    .map_err(|error| format!("Cannot fetch the picture {} painted: {error}", painter.name))?;
                if !response.status().is_success() {
                    return Err(format!(
                        "Cannot fetch the picture {} painted: HTTP {}",
                        painter.name,
                        response.status().as_u16()
                    ));
                }
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|error| format!("The picture broke off: {error}"))?;
                Ok(bytes.to_vec())
            };
            let bytes = tokio::select! {
                outcome = fetched => outcome?,
                () = cancel.cancelled() => return Err("The painting was cancelled.".to_string()),
            };
            if bytes.len() > PICTURE_BYTES {
                return Err(format!(
                    "The picture {} painted is larger than {} MB.",
                    painter.name,
                    PICTURE_BYTES / 1024 / 1024
                ));
            }
            let extension =
                kind_of(&bytes).ok_or_else(|| format!("What {} sent back is not a picture.", painter.name))?;
            Ok(Painted { bytes, extension, size })
        }
    }
}

/// A picture's kind, from its first bytes: the extension it is saved with.
pub fn kind_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// Standard base64, as the Images API sends a picture; `None` for anything else.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let body = clean
        .strip_suffix(b"==")
        .or_else(|| clean.strip_suffix(b"="))
        .unwrap_or(&clean);
    if body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.chunks(4) {
        let mut word = 0u32;
        for (index, c) in chunk.iter().enumerate() {
            word |= value(*c)? << (18 - 6 * index);
        }
        out.push((word >> 16) as u8);
        if chunk.len() > 2 {
            out.push((word >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(word as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A server that answers each request it gets with the next of `answers`, and keeps what it was sent.
    pub(crate) fn serve(answers: Vec<(u16, &'static str, Vec<u8>)>) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (sent, got) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, kind, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 65536];
                // Headers, then as much body as Content-Length says.
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if read == 0 {
                        break;
                    }
                }
                // Kept for whoever wants to look at it; a test that does not is not told.
                let _ = sent.send(String::from_utf8_lossy(&request).to_string());
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (address, got)
    }

    pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest of a picture";

    #[test]
    fn reads_base64_as_the_images_api_sends_it() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", PNG] {
            assert_eq!(unbase64(&crate::events::base64(bytes)).as_deref(), Some(bytes));
        }
        assert_eq!(unbase64("aGVs\nbG8="), Some(b"hello".to_vec()));
        assert_eq!(unbase64("not base64!"), None);
    }

    #[test]
    fn asks_each_painter_for_a_size_it_lists() {
        let sizes: Vec<&str> = PAINTERS
            .iter()
            .flat_map(|painter| SHAPES.iter().map(move |shape| size(painter, shape)))
            .collect();
        assert_eq!(sizes[..3], ["1024x1024", "1536x1024", "1024x1536"]);
        assert_eq!(sizes[3..6], ["1280x1280", "1568x1056", "1056x1568"]);
        assert_eq!(sizes[6..9], ["1024x1024", "1344x768", "768x1344"]);
    }

    #[tokio::test]
    async fn takes_openais_picture_from_the_answer_and_zhipus_from_its_address() {
        let answer = format!(
            r#"{{"created":1,"data":[{{"b64_json":"{}"}}]}}"#,
            crate::events::base64(PNG)
        );
        let (address, sent) = serve(vec![(200, "application/json", answer.into_bytes())]);
        let config = ImageConfig {
            painter: painter("gpt-image-2").unwrap(),
            url: format!("{address}/v1/images/generations"),
            key: " sk-test ".into(),
        };
        let painted = paint(&config, "一只白鹤", "landscape", true, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            (painted.bytes.as_slice(), painted.extension, painted.size),
            (PNG, "png", "1536x1024")
        );
        let request = sent.recv().unwrap();
        assert!(
            request.starts_with("POST /v1/images/generations ")
                && request.to_lowercase().contains("authorization: bearer sk-test\r\n")
        );
        let body: Value = serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(
            body,
            json!({ "model": "gpt-image-2", "prompt": "一只白鹤", "size": "1536x1024", "quality": "high", "n": 1, "output_format": "png" })
        );

        // Zhipu answers with an address, which is fetched at once.
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3].to_vec();
        let (picture_at, _) = serve(vec![(200, "image/jpeg", jpeg.clone())]);
        let answer = format!(r#"{{"created":1,"data":[{{"url":"{picture_at}/files/a.jpg"}}],"content_filter":[]}}"#);
        let (address, sent) = serve(vec![(200, "application/json", answer.into_bytes())]);
        let config = ImageConfig {
            painter: painter("cogview-4").unwrap(),
            url: format!("{address}/api/paas/v4/images/generations"),
            key: "glm".into(),
        };
        let painted = paint(&config, "山水", "square", false, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!((painted.bytes, painted.extension), (jpeg, "jpg"));
        let request = sent.recv().unwrap();
        let body: Value = serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(
            body,
            json!({ "model": "cogview-4-250304", "prompt": "山水", "size": "1024x1024", "quality": "standard" })
        );
    }

    #[tokio::test]
    async fn says_what_the_service_said_when_it_would_not_paint() {
        let (address, _sent) = serve(vec![(
            400,
            "application/json",
            br#"{"error":{"code":"1301","message":"content not allowed"}}"#.to_vec(),
        )]);
        let config = ImageConfig {
            painter: painter("glm-image").unwrap(),
            url: format!("{address}/images/generations"),
            key: "glm".into(),
        };
        let error = paint(&config, "x", "square", false, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            error.contains("1301") && error.contains("content not allowed"),
            "{error}"
        );
    }
}
