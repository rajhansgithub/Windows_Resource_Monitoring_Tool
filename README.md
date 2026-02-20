<img width="481" height="46" alt="image" src="https://github.com/user-attachments/assets/6bd7cfaf-87c2-47c8-8ae2-9268371ade08" />

# Windows Network Monitor

A lightweight, open-source Windows desktop application designed for real-time system monitoring. It provides an unobtrusive, always-on-top overlay that tracks network speed (Upload/Download), total data usage, CPU load, and GPU utilization. Ideally suited for power users who want minimal distraction but maximum visibility into their system's performance.

## Key Features

### 🚀 Real-time Monitoring
- **Network Speed**: Instant visualization of Upload and Download speeds.
- **Total Usage Tracking**:
  - Displays today's total data usage directly on the main widget.
  - Keeps a persistent history of Daily, Monthly, and Yearly data consumption.
- **Hardware Stats**:
  - **CPU Usage**: Real-time percentage load.
  - **GPU Usage**: Monitoring for single or multiple GPUs.

### 📊 Comprehensive Usage Reports
- **Detailed History**: View your data consumption trends over time.
- **Report Window**: Dedicated interface to analyze Daily, Monthly, Yearly, and All-Time usage statistics.

### 🛠️ Customization & Control
- **System Tray Integration**: Full control via a right-click context menu.
- **Toggle Visibility**: Show or hide any component (CPU, GPU, Network, Total Usage).
- **Flexible Units**: Switch between KB/s, MB/s, Kbps, and Mbps.
- **Adapter Selection**: Choose specific network interfaces to monitor.
- **Startup Manager**: Built-in option to launch automatically with Windows.

## Installation

### From Release
1.  Go to the [Releases](../../releases) page.
2.  Download the latest installer or portable executable.
3.  Run the application. It will minimize to the system tray and display the widget near your taskbar.

### Building from Source

**Prerequisites**:
- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
- [Inno Setup 6+](https://jrsoftware.org/isdl.php) (optional, for installer creation)

**Steps**:
1.  Clone the repository:
    ```bash
    git clone https://github.com/rajhansgithub/Windows_Resource_Monitoring_Tool.git
    cd Windows_Resource_Monitoring_Tool
    ```
2.  Build the project:
    ```bash
    .\publish.bat
    ```
3.  The executable will be available in the `bin/Release` or `publish` directory.

## Usage

- **Right-click the System Tray Icon** to:
  - **View**: Toggle visibility of CPU, GPU, Network, and Total Usage widgets.
  - **Usage Report**: Open the detailed data usage history window.
  - **Speed Unit**: Switch between bits and bytes (e.g., Mbps vs MB/s).
  - **Adapters**: Select the active network interface.
  - **Run at Startup**: Toggle auto-start behavior.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
