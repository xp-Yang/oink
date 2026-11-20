# 双机共用显示器

这是基于 Oink v0.3.7 的修改版。它可以让已有的「将指针移动到某台设备」快捷键同时切换共用显示器的输入源。比如 `Ctrl+Alt+1` 固定切到 Windows，`Ctrl+Alt+2` 固定切到 Mac。一套键鼠一直连在同一台主机上。

## 先测试两个方向

DDC/CI 指令由**实际连接键盘的电脑**发出。先在这台电脑上手动测试切到另一输入，再用**同一台电脑**的命令切回来。两台电脑都应该开机、联网并运行 Oink。

- **Apple 芯片 Mac**使用 [m1ddc](https://github.com/waydabber/m1ddc)。用 `command -v m1ddc` 确定完整路径，用 `m1ddc display list` 找到显示器的 UUID 或序号。例如 `m1ddc display 1 set input 17`。已经验证过 `input-alt` 的显示器可以用 `m1ddc-alt` 后端。
- **Windows**使用 [ControlMyMonitor](https://www.nirsoft.net/utils/control_my_monitor.html)。从工具窗口取得显示器 ID 或序列号。例如在 PowerShell 运行 `& 'C:\Tools\ControlMyMonitor.exe' /SetValue '显示器序列号' 60 17`。

例子里的输入编号只便于演示。请填写该显示器实际验证过的数字。部分显示器在切走之后不接受原端口的 DDC 指令，这种情况下固定接在这一端的键盘无法用本版实现双向切换。先验证两次实际画面，再启用自动联动。

在 Mac 上可以把两条命令连起来，例如 `m1ddc display 1 set input 17 && sleep 5 && m1ddc display 1 set input 15`。这样能直接验证 Mac 在失去画面后能否把显示器切回。失败时可以用显示器遥控器恢复。

## 配置快捷键

1. 在 Oink 的 **设置 → 快捷键**，为两台设备分别配置「将指针移动到」快捷键。Mac 键盘的 Alt 对应 Option。键盘连接在哪台电脑，就在哪台电脑上配置。
2. 在连接实体键盘的电脑上，按下面的样式新建 `ddc.json`。Mac 的路径是 `~/Library/Application Support/Oink/ddc.json`，Windows 的路径是 `%LOCALAPPDATA%\Oink\ddc.json`。如果设置了 `OINK_HOME`，文件放在该目录下。
3. 把 `inputs` 中的键改成 Oink 显示的实际设备名，值改成该主机接入 P27D 的输入编号。设备重名时可以用完整的 64 字符小写十六进制设备 ID 作键，优先于设备名。把 `program` 改为本机工具的绝对路径。
4. 彻底退出并重开修改版。按两个快捷键分别测试画面、鼠标和键盘。切换时，Oink 会等 DDC 命令结束，再请求转移键鼠并检查焦点。如果工具报错或超时，Oink 会保持原有键鼠控制，并在后端日志记录原因。工具执行成功后，实际画面仍以屏幕显示为准。

Mac 示例：

```json
{
  "backend": "m1ddc",
  "program": "/opt/homebrew/bin/m1ddc",
  "display": "1",
  "inputs": {
    "我的 Windows": 17,
    "我的 Mac": 15
  }
}
```

Windows 示例：

```json
{
  "backend": "control-my-monitor",
  "program": "C:\\Tools\\ControlMyMonitor.exe",
  "display": "实际显示器序列号",
  "inputs": {
    "我的 Windows": 17,
    "我的 Mac": 15
  }
}
```

这项功能只在键盘所在主机上运行，另一台可以继续用同版本的原版 Oink。没有匹配映射的设备继续使用原版键鼠快捷键。鼠标跨屏、音频转发遵循 Oink 原来的设置。删除或改名 `ddc.json` 后重启，即可关闭 DDC 联动。

## 源码与验证

在 v0.3.7 上应用补丁。可用 `cargo test --locked -p oink-agent --lib ddc::tests` 验证指令构造、执行失败和超时。原项目的桌面打包入口是 `build-support/package.py`，Windows 和 macOS 包需要在相应系统构建。官方 macOS 打包流程还依赖上游的 Developer ID 签名配置，分发修改版需配置自己的签名与分发渠道。
