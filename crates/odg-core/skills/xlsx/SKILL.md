---
name: xlsx
description: 做、改、读 Excel 表格（.xlsx）：数据整理、统计、带公式的报表、预算表。要交出一份表格，或者要改、要分析一份 .xlsx、.csv 时用
---

# Excel 表格

用文档运行环境里的 openpyxl（要读写格式和公式）或 pandas（要整理、统计数据），写一个 Python 脚本生成或修改，再交给本尊。

## 1. 备好环境

- 先调用 `document_runtime`，拿到它的 Python 路径；说没装，就用 `install: true` 再调一次（会先请示本尊）。
- 只用这个 Python 跑脚本，命令照 `document_runtime` 给出的写法原样用（Windows 上开头有 `&`，少了 PowerShell 会报错），例如 `& "<python>" make_sheet.py`。不要往别的 Python 里装库，也不要对它跑 pip。

## 2. 写脚本

- 脚本放在洞府里，按用途取名，不要叫 `openpyxl.py`、`pandas.py`。
- 读：`pandas.read_excel(路径, sheet_name=None)` 一次读出所有工作表；要看公式本身用 `openpyxl.load_workbook(路径)`，要看算好的值用 `load_workbook(路径, data_only=True)`（只有 Excel 存过的文件才有算好的值）。
- 改已有的文件：用 openpyxl 打开，另存成新文件，除非本尊让你覆盖。pandas 写回会丢掉格式和公式，不要用它改别人的表。
- 算出来的数用公式写，不要把 Python 算好的结果写死：`ws["B10"] = "=SUM(B2:B9)"`。这样数据变了表还能自己算。
- openpyxl 只把公式当文本写进去，不会算出结果。写完设上打开时重算，Excel、WPS 打开时就会算：

```python
wb.calculation.fullCalcOnLoad = True
```

- 版面：表头加粗、冻结首行（`ws.freeze_panes = "A2"`）、列宽按内容设（`ws.column_dimensions["A"].width = 14`）；金额、百分比设好数字格式（`cell.number_format = "#,##0.00"`、`"0.0%"`）。
- 日期写成 `datetime`，不要写成文字。

## 3. 核对

- 用同一个 Python 重新打开，检查每个工作表的行列数、表头、几个关键单元格。
- 有公式、又装了 LibreOffice 的话，让它算一遍再查：`& "<soffice>" --headless --convert-to xlsx --outdir <临时目录> 文件.xlsx`，用 `load_workbook(临时目录里的文件, data_only=True)` 读出算好的值，查有没有 `#REF!`、`#DIV/0!`、`#NAME?`、`#VALUE!` 这类错误，结果对不对得上。不要用这份临时文件替换原文件。没装 LibreOffice 就跳过，并告诉本尊公式没有实算核对。

## 4. 交付

用 `deliver_files` 把做好的 .xlsx 交给本尊，说清楚有哪些工作表、哪些是公式。
