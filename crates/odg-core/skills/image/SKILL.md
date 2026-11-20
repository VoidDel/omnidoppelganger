---
name: image
description: 画图：数据图表、流程图、架构图、示意图、图标、标志、海报、卡片、封面、二维码、几何或生成式图案，给图片加字、拼图、改尺寸，以及请图像模型画照片、写实渲染、插画。要交出一张图，或者要往 Word、PPT、网页里放图时用
---

# 画图

能用代码画的都用代码画：画出来的东西准、能改、字一个不错，也不花钱。矢量图写 SVG；位图用文房的 Python（先调用 `document_runtime`，命令照它给的写法跑，Windows 上开头有 `&`）。代码画不出来的（照片、写实渲染、插画、人物场景），才用 `generate_image` 请图像模型作画，见第 6 节。

## 1. 选画法

| 要画的 | 画法 |
|---|---|
| 柱状、折线、饼图、散点等数据图表 | matplotlib，存 PNG（放进文档）或 SVG（放网页） |
| 流程图、架构图、时序、示意图、思维导图 | 直接写 SVG；要 PNG 时用 resvg 转 |
| 图标、标志、徽章 | SVG |
| 海报、封面、卡片、横幅 | SVG 排版，用 resvg 转成 PNG |
| 二维码 | qrcode |
| 几何图案、生成式图案、纹理 | Python 算出 SVG，或用 Pillow 直接画 |
| 给已有的图加字、拼图、裁剪、改尺寸、加边框 | Pillow，另存新文件 |
| 照片、写实渲染、产品效果图、插画、人物、场景、材质贴图 | `generate_image` 作画 |

两样都要的，分开做：画面用 `generate_image`，字和排版用代码叠上去（模型画的字常常写错，尤其是中文）。工具里没有 `generate_image`，说明本尊还没设好画师：告诉本尊到设置的"神通 → 丹青"里看，不要用代码硬凑一张照片。

## 2. 先定版面

- 先想清楚：给谁看、放在哪（PPT 一页、Word 一节、网页横幅）、尺寸多大。PPT 16:9 用 1920×1080，Word 版心宽约 16 cm（PNG 宽 1600 px 左右），图标 24×24 或 512×512，社交封面 1200×630。
- 一张图只说一件事；字少而大，标题一句结论。
- 配色：一个主色、一个强调色，其余用深浅灰；背景浅色，字深色，对比要够。同一组图用同一套颜色和字号。
- 留白：四周留出画面的 5% 以上，元素对齐到同一条线上。

## 3. SVG

- 写上 `viewBox` 和 `width`、`height`，比如 `viewBox="0 0 1200 630" width="1200" height="630"`。
- 中文字体写成 `font-family="Microsoft YaHei, PingFang SC, Noto Sans CJK SC, sans-serif"`；居中的字用 `text-anchor="middle"`，一行字估宽度：中文字宽约等于字号。
- 箭头用 `<marker>`；同样的方框、颜色用 `<style>` 或 `<defs>` 定义一次。
- 写好的 .svg 在对话里直接显示，打开看过再交。
- 要 PNG（放进 Word、PPT，或者对方不认 SVG）时用 resvg 转，它用电脑上已有的字体，中文能正常显示：

```python
import resvg_py
png = resvg_py.svg_to_bytes(svg_path="flow.svg", zoom=2)   # 2 倍清晰；也可以给 width=1600
open("flow.png", "wb").write(png)
```

  透明背景的 SVG 要白底时加 `background="#ffffff"`。

## 4. 数据图表（matplotlib）

```python
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
plt.rcParams["font.sans-serif"] = ["Microsoft YaHei", "SimHei", "PingFang SC", "Noto Sans CJK SC", "DejaVu Sans"]
plt.rcParams["axes.unicode_minus"] = False
```

- 不设字体，中文会变成方块。
- 数据从文件里读（pandas），不要手抄数字；数字在图上标出来。
- 去掉上边和右边的框线（`ax.spines[["top", "right"]].set_visible(False)`），标题写结论，坐标轴写单位。
- 存图：`fig.savefig("chart.png", dpi=200, bbox_inches="tight")`；要矢量就存 `.svg`。
- 一张图一个脚本，脚本名按用途取（`make_chart.py`），不要叫 `matplotlib.py`。

## 5. 位图（Pillow）和二维码

- 中文字体：Windows 用 `ImageFont.truetype("C:/Windows/Fonts/msyh.ttc", 字号)`，macOS 用 `/System/Library/Fonts/PingFang.ttc`，Linux 找 Noto Sans CJK。
- 改图另存新文件，不盖掉原图；缩小用 `Image.LANCZOS`。
- 二维码：`qrcode.make("网址").save("qr.png")`；要放在海报上，生成后用 Pillow 贴进去，四周留白不少于码宽的十分之一。

## 6. 作画（generate_image）

- 用的是设置里"神通 → 丹青"选的画师，每张都先请示本尊；代码画得出的部分，不要拿来作画。
- 提示词写全，不要只写几个词。按这几项写，没有的略过：用途（封面、横幅、贴图……）；主体，在做什么；风格或媒介（写实摄影、水彩、工笔、扁平插画、3D 渲染……）；构图和景别；光线和氛围；配色和材质；图上的字（原样写出，用引号括起来，能不要字就不要字）；要避开的（水印、多余的手指、杂乱的背景……）。
- `shape`：横幅和封面用 `landscape`，海报和手机竖屏用 `portrait`，头像和图标用 `square`。`quality`：先用 `standard` 出稿，本尊认可了再用 `high` 出终稿。
- 一次一张；要几个方案就调几次，每张换一个文件名。文件放在洞府里，没有指定就放在 `images/` 下，按内容取名。已有的文件不会被盖掉，改版用新名字，如 `cover-v2.png`。
- 你看不到画出来的图：不要说它画得怎样，只说用了哪段提示词、存在哪里，请本尊看过再定。改图就写一段新的提示词，写明"只改 X，其余保持不变"，并告诉本尊改出来的是另一张图，构图可能变。
- 不画冒充真人的照片，不仿冒别人的商标。服务方拒绝画时，照实告诉本尊它说了什么。

## 7. 核对和交付

- 用同一个 Python 打开结果，查尺寸对不对（`Image.open(path).size`）。你看不到图的样子：坐标和字号要算清楚，字不要出界、不要重叠。
- SVG、`write_file` 写的图和 `generate_image` 画的图会显示在对话里；脚本画出的 PNG，用 `deliver_files` 交给本尊。
- 最后说清楚每张图的路径、尺寸、用途、画法（代码画的说改哪个脚本或文件，作画的给出提示词），以及作画调用了几次。
