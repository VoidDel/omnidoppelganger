//! A one-shot local HTTP server for testing the streaming clients end to end.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Answer one connection per entry of `responses`, in order, each with the
/// raw HTTP response given, then close it. Resolves to the raw requests received.
pub async fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    serve_bytes(responses.into_iter().map(String::into_bytes).collect()).await
}

/// [`serve`], for responses whose bodies are not text: a PDF, a gzipped page.
pub async fn serve_bytes(responses: Vec<Vec<u8>>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut socket).await);
            socket.write_all(&response).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    });
    (address, task)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = socket.read(&mut buffer).await.unwrap();
        bytes.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&bytes).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap()))
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length || read == 0 {
                return text;
            }
        } else if read == 0 {
            return text;
        }
    }
}

/// A streamed `200` whose body is the given server-sent events, one `data:` line each.
pub fn event_stream(events: &[serde_json::Value], done: bool) -> String {
    let mut body: String = events.iter().map(|event| format!("data: {event}\n\n")).collect();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

/// A JSON error response with the given status.
pub fn error(status: u16, body: &str) -> String {
    format!("HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
}
