---
name: pdf
description: 做 PDF，或者拆、合、读、改 PDF：生成报告、把几份 PDF 合成一份、取出某几页、加页码水印、取出文字。要交出一份 PDF，或者要处理已有的 PDF 时用
---

# PDF

生成用文档运行环境里的 reportlab，拆合、读取用 pypdf。写一个 Python 脚本来做，再交给本尊。

## 1. 备好环境

- 先调用 `document_runtime`，拿到它的 Python 路径；说没装，就用 `install: true` 再调一次（会先请示本尊）。
- 只用这个 Python 跑脚本，命令照 `document_runtime` 给出的写法原样用（Windows 上开头有 `&`，少了 PowerShell 会报错），例如 `& "<python>" make_pdf.py`。不要往别的 Python 里装库。

## 2. 要的是 Word、Excel、PPT 的 PDF 版

先用对应的秘籍（docx、xlsx、pptx）做出原文件。装了 LibreOffice 的话再转：`& "<soffice>" --headless --convert-to pdf --outdir <目录> 文件`。没装就交原文件，告诉本尊用 Office 另存为 PDF。

## 3. 用 reportlab 直接生成

- 用 `platypus`（`SimpleDocTemplate`、`Paragraph`、`Table`、`Spacer`、`PageBreak`）排版，让它自己分页，不要用 `canvas` 一行行算坐标。
- 中文必须注册中文字体，否则全是方块。最稳的是 reportlab 自带的、不用字体文件的：

```python
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.cidfonts import UnicodeCIDFont
pdfmetrics.registerFont(UnicodeCIDFont("STSong-Light"))
```

  然后在样式里用 `fontName="STSong-Light"`。要好看些、又确定本尊电脑上有，也可以注册 TrueType 字体：Windows 的 `C:/Windows/Fonts/simhei.ttf`（`TTFont("SimHei", 路径)`）。不要用 .otf 字体，reportlab 读不了。
- 中文段落要能断行：段落样式设 `wordWrap="CJK"`。
- A4：`pagesize=A4`，页边距 2 cm 左右；页码在 `onPage` 回调里画。

## 4. 拆、合、取页、取文字

- 合并：`PdfWriter()` 逐个 `append(路径)`；取页：`PdfReader` 读，`writer.add_page(reader.pages[i])`。
- 取文字：`page.extract_text()`；扫描件取不出字，告诉本尊这是图片，不要编内容。
- 不要覆盖原文件，另存成新文件，除非本尊让你覆盖。

## 5. 核对

用 pypdf 重新打开生成的文件：页数对不对，每页取出的文字里中文是不是都在（没有乱码、没有方块对应的空白），表格有没有被截断。

## 6. 交付

用 `deliver_files` 把做好的 PDF 交给本尊，说清楚页数和内容。
