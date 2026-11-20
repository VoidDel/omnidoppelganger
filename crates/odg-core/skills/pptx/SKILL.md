---
name: pptx
description: 做、改、读 PowerPoint 演示文稿（.pptx）：汇报、方案、课件、路演。要交出一份 PPT，或者要改、要读一份 .pptx 时用
---

# PowerPoint 演示文稿

用文档运行环境里的 python-pptx，写一个 Python 脚本生成或修改，再交给本尊。

## 1. 备好环境

- 先调用 `document_runtime`，拿到它的 Python 路径；说没装，就用 `install: true` 再调一次（会先请示本尊）。
- 只用这个 Python 跑脚本，命令照 `document_runtime` 给出的写法原样用（Windows 上开头有 `&`，少了 PowerShell 会报错），例如 `& "<python>" make_deck.py`。不要装 npm 包，不要往别的 Python 里装库。

## 2. 先定内容，再排版

- 先列出每一页讲什么：一页一个意思，标题是一句结论（"三季度成本降了 12%"），不是一个词（"成本"）。
- 正文每页不超过五六行，每行一句话；细节放进备注（`slide.notes_slide.notes_text_frame.text`）。

## 3. 写脚本

- 脚本放在洞府里，按用途取名，不要叫 `pptx.py`。
- 新建用 `Presentation()`，设成 16:9：`prs.slide_width = Inches(13.333)`、`prs.slide_height = Inches(7.5)`。照着已有的文件或模板做，就打开它，用它的版式（`prs.slide_layouts`），另存成新文件。
- 用版式里的占位符放标题和正文（`slide.shapes.title`、`slide.placeholders[1]`），少用到处摆的文本框；同一类页面位置一致。
- 字体：中文设成本尊电脑上有的字体，如"微软雅黑"；字号标题 28–40、正文 16–24，不要为了塞下更多字缩小字号，拆成两页。
- 颜色一份配色用到底：一个主色、一个强调色、深灰的字，背景浅色。
- 表格用 `shapes.add_table`，图表用 `shapes.add_chart`（`pptx.chart.data.CategoryChartData`），图片用 `add_picture` 并保持比例。

## 4. 核对

- 用同一个 Python 重新打开，逐页取出文字，检查每页都有标题，没有空页，文字没有乱码。
- 估一估文字会不会溢出：文本框里的行数乘字号，超出框的高度就拆页或删字。
- 装了 LibreOffice 的话，转成 PDF：`& "<soffice>" --headless --convert-to pdf --outdir <临时目录> 文件.pptx`，用 pypdf 检查页数对得上。LibreOffice 没有本尊的字体时会换字体，版面略有出入属正常。没装就跳过，并告诉本尊没有做版面核对。

## 5. 交付

用 `deliver_files` 把做好的 .pptx 交给本尊，列出每页的标题。
