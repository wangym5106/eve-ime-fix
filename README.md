# EVE输入法修复工具

修复 EVE Online 客户端中不显示输入法候选框的问题。

本工具不注入代码、不读写目标进程内存、不模拟键盘或鼠标输入、不修改任何游戏文件、不参与游戏逻辑、不读取游戏数据。

用法：运行 [eve-ime-fix.exe](https://github.com/wangym5106/eve-ime-fix/releases/latest/download/eve-ime-fix.exe) 即可。程序启动后在后台常驻并在通知区域显示托盘图标。

诊断：运行日志位于 `%LOCALAPPDATA%\eve-ime-fix\eve-ime-fix.log`。

## 工作原理

EVE Online 客户端有两处与输入法相关的实现缺陷。本工具修复了第一处，第二处无法在游戏外修复。

### 缺陷一：客户端吞掉了输入法界面显示通知

Windows 只有在窗口把 `WM_IME_SETCONTEXT` 交给默认处理流程 `DefWindowProc` 之后，才允许输入法绘制自己的界面（候选框）。EVE客户端在 `carbonui.services.ime.windowshandler` 的 `WindowsImeHandler._OnSetContext` 中无条件返回 `True`，引擎据此把该消息标记为"已处理"，不再向下传递，输入法因此不再绘制候选框。

本工具在进程外向EVE窗口内的输入法窗口补发该通知：`SendMessageTimeoutW(WM_IME_SETCONTEXT, wParam=1, lParam=0xC000000F)`，使输入法能够正常绘制候选界面。

### 缺陷二：客户端从不向输入法报告插入位置

客户端的输入法处理链只维护游戏自绘的组合串窗口，从未设置输入法的定位信息，输入法因此无从得知文字插入位置，候选框也只能落在自身的默认位置，即窗口右下角。

该缺陷只能由FC在游戏内进行修复，无法在外部完成。

### 关于向官方提交 Bug Report

两处缺陷都出在客户端自身，且都属于应当修复而长期未修的实现问题：其一忽略了系统下发的界面授权通知，其二从未向输入法报告插入位置。完整的修复需要客户端同时满足：

- `_OnSetContext` 不再把 `WM_IME_SETCONTEXT` 报告为已处理
- 在使用输入法输入文本时调用 `ImmSetCompositionWindow` / `ImmSetCandidateWindow`，把插入位置告知输入法

**因此建议向 FC 提交 Bug Report**，以推动从源头修复。报告建议包含以下内容：

- 现象：使用微软拼音等输入法时，客户端输入框仅显示输入的拼音串，候选词列表不出现，只能通过空格与数字键盲选。
- 复现：使用输入法在游戏内输入文本，候选框不显示
- 环境：Windows 版本、输入法名称与版本
- 参考：外部程序补发 `WM_IME_SETCONTEXT` 后候选框即可显示，说明问题出在客户端对该通知的处理上；补发后候选框固定在客户区右下角，说明定位信息从未被设置

## 免责声明

- 本项目与 FC/CCP 无任何关联，非官方工具。
- 本工具仅调用 Windows 公开的系统接口，不读取游戏数据、不注入游戏进程、不改动游戏文件。请自行判断是否使用，并自行承担由此产生的风险。

## 开源协议

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。
