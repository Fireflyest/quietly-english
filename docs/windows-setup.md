# Windows 编译环境准备

`quietly` 依赖 `rusqlite` 的 `bundled` 特性，会用 C 编译器编译 SQLite；
Rust 还需要一个系统链接器。所以 Windows 上必须二选一：

- **方案 A（推荐，体积小）**：MinGW-w64 + GNU 工具链
- **方案 B（体积大，但和你已装的 MSVC 工具链匹配）**：Visual Studio Build Tools + MSVC 工具链

装好任意一个，回到项目目录 `cargo build` 即可。

---

## 方案 A：MinGW-w64（GNU 工具链）

你的 Rust 默认工具链已经是 `stable-x86_64-pc-windows-gnu`，只缺 `gcc`、`dlltool`、`ld`。

### 下载

**推荐：WinLibs 官方发布页**（从列表里选 `x86_64-posix-seh-gcc-*-mingw-w64ucrt-*.zip`，
**不要选带 `llvm` 的那个**，体积会大 200MB）：

- <https://github.com/brechtsanders/winlibs_mingw/releases/latest>
- 直链示例（版本号可能已更新，以发布页为准）：
  `https://github.com/brechtsanders/winlibs_mingw/releases/download/16.2.0posix-23.1.3-14.0.0-ucrt-r3/winlibs-x86_64-posix-seh-gcc-16.2.0-mingw-w64ucrt-14.0.0-r3.zip`

国内访问 GitHub 慢的话，用下面任意一个代理前缀拼在原链接前面：

```
https://ghfast.top/<原始 GitHub 链接>
https://gh-proxy.com/<原始 GitHub 链接>
https://ghproxy.net/<原始 GitHub 链接>
```

其他可选来源：

- MSYS2（装完在 MSYS2 里 `pacman -S mingw-w64-ucrt-x86_64-gcc`）<https://www.msys2.org/>
- WinLibs 官网（含 7z 压缩版，更小）<https://winlibs.com/>

### 安装

1. 解压到例如 `E:\tools\mingw64`，确认存在 `E:\tools\mingw64\bin\gcc.exe`
2. 把 `E:\tools\mingw64\bin` 加入 PATH（当前会话临时生效）：

   ```bash
   export PATH=/e/tools/mingw64/bin:$PATH
   gcc --version && dlltool --version
   ```

   永久生效：`系统属性 → 环境变量 → Path → 新建`，或用 PowerShell：

   ```powershell
   [Environment]::SetEnvironmentVariable("Path", $env:Path + ";E:\tools\mingw64\bin", "User")
   ```

3. 可选：在项目里固定工具链（团队协作更稳）

   ```toml
   # .cargo/config.toml
   [build]
   target = "x86_64-pc-windows-gnu"
   ```

### 验证

```bash
cd /e/projects/rust/quietly-english
cargo build
cargo test
```

---

## 方案 B：MSVC

你已经装了 `stable-x86_64-pc-windows-msvc`，但机器上没有 VS Build Tools，
`link.exe` 还被 Git 的 `/usr/bin/link.exe` 抢先了。装完 Build Tools 后要确保
**MSVC 的 link.exe 在 PATH 里排在 Git 之前**（用 “x64 Native Tools 命令提示符” 最稳）。

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools `
  --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

或手动下载（勾选 “使用 C++ 的桌面开发”）：

- <https://visualstudio.microsoft.com/visual-cpp-build-tools/>

然后：

```bash
rustup default stable-x86_64-pc-windows-msvc
cargo build
```

---

## 常见报错对照

| 报错 | 原因 | 处理 |
| --- | --- | --- |
| `linker 'gcc' not found` | GNU 工具链缺 MinGW | 装方案 A |
| `error calling dlltool 'dlltool.exe': program not found` | 同上（`windows-targets` 需要 dlltool） | 装方案 A |
| `link: extra operand ...` | MSVC 工具链用了 Git 的 `link.exe` | 用 `x64 Native Tools` 终端，或方案 A |
| `failed to find tool "cl.exe"` | 缺 VS Build Tools | 装方案 B |
| `error: could not compile libsqlite3-sys` | 没有 C 编译器 | 两个方案都行 |
