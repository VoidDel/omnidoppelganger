---
name: docx
description: 做、改、读 Word 文档（.docx）：报告、方案、合同、说明书、公文。要交出一份 Word 文件，或者要改、要读一份 .docx 时用
---

# Word 文档

用文档运行环境里的 python-docx，写一个 Python 脚本生成或修改，再交给本尊。

## 1. 备好环境

- 先调用 `document_runtime`，拿到它的 Python 路径；说没装，就用 `install: true` 再调一次（会先请示本尊）。
- 只用这个 Python 跑脚本，命令照 `document_runtime` 给出的写法原样用（Windows 上开头有 `&`，少了 PowerShell 会报错），例如 `& "<python>" make_report.py`。不要往系统的 Python 里装库，也不要对它跑 pip。

## 2. 写脚本

- 脚本放在洞府里，按用途取名（`make_report.py`），不要叫 `docx.py`，那会遮住库本身。
- 新建用 `Document()`；照着已有的文件改，就 `Document("原文件.docx")` 打开，另存成新文件，除非本尊让你覆盖。
- 结构用内置样式：`add_heading(文字, level)`、`add_paragraph(文字, style="List Bullet")`、`add_table(rows, cols)` 后设 `table.style = "Table Grid"`。标题层级要连贯，不跳级。
- 中文字体要单独设东亚字体，否则 Word 会用默认西文字体显示中文：

```python
from docx.oxml.ns import qn
style = document.styles["Normal"]
style.font.name = "微软雅黑"
style.element.rPr.rFonts.set(qn("w:eastAsia"), "微软雅黑")
```

  标题样式（`Heading 1` 等）同样设一遍。
- 页面：`section.page_width/height`、页边距用 `docx.shared.Cm`；A4 是 21×29.7 cm。
- 图片：`add_picture(路径, width=Cm(15))`，不要超过版心宽度。
- 分页用 `add_page_break()`，不要用一串空段落顶到下一页。

## 3. 核对

- 用同一个 Python 重新打开生成的文件，数一数标题、段落、表格是不是都在，文字有没有错字乱码。
- `document_runtime` 说装了 LibreOffice 的话，把文件转成 PDF：`& "<soffice>" --headless --convert-to pdf --outdir <临时目录> 文件.docx`，再用 pypdf 看页数、取出文字检查有没有串行、缺字。没装就跳过这一步，并告诉本尊没有做版面核对。

## 4. 交付

用 `deliver_files` 把做好的 .docx 交给本尊，说清楚放在哪、里面有什么。
