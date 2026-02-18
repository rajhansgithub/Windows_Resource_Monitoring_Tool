# Windows Network Monitor

A lightweight, open-source Windows desktop application that monitors real-time network usage (Upload/Download speeds), CPU usage, and GPU usage. It resides quietly in your system tray and provides an unobtrusive overlay or taskbar-anchored widget.

## Features

- **Real-time Monitoring**:
  - Network Upload/Download speeds.
  - CPU Usage (%).
  - GPU Usage (%).
- **Minimalist UI**: unobtrusive dark-themed widget that stays on top or anchors near the taskbar.
- **System Tray Integration**:
  - Toggle visibility of CPU, GPU, or Network stats independently.
  - Select specific network adapters.
  - Change speed units (KB/s, MB/s, Kbps, Mbps).
  - Enable/Disable "Run at Startup".
- **Single File**: Runs as a standalone executable (after installation/setup).

## Installation

### From Release
1.  Go to the [Releases](../../releases) page (link to be added).
2.  Download `Setup_ResourceMonitorTool.exe`.
3.  Run the installer and follow the instructions.
4.  The app will launch and minimize to the system tray.

### Building from Source

**Prerequisites**:
- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
- [Inno Setup 6+](https://jrsoftware.org/isdl.php) (for creating the installer)

**Steps**:
1.  Clone the repository:
    ```bash
    git clone https://github.com/rajhansgithub/Windows_Resource_Monitoring_Tool.git
    cd Windows_Resource_Monitoring_Tool
    ```
2.  Build and Create Installer:
    ```bash
    .\publish.bat
    ```
    This script will:
    - Build the .NET application (Release mode, Single File).
    - Run Inno Setup Compiler (`ISCC`) to generate the `Setup_ResourceMonitorTool.exe`.
3.  Find the installer in the `Installer` directory.

## Usage

- **Right-click the System Tray Icon** to:
  - Select your active network adapter.
  - Toggle CPU/GPU/Network visibility.
  - Change speed units.
  - Enable/Disable startup.
  - Exit the application.
- The widget automatically positions itself near the notification area (system tray).

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
