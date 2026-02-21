using System;
using System.Diagnostics;
using System.Drawing; // For Icon
using System.Linq;
using System.Windows;
using System.Windows.Threading;
using WindowsNetworkMonitor.Interop;
using WindowsNetworkMonitor.Services;
using Application = System.Windows.Application;
// We will use fully qualified names for WinForms to avoid collisions
// using System.Windows.Forms; 

namespace WindowsNetworkMonitor
{
    public partial class MainWindow : Window
    {
        private readonly NetworkMonitor _networkMonitor;
        private readonly HardwareMonitor _hardwareMonitor;
        private readonly ProcessMonitor _processMonitor;
        private readonly NetworkUsageManager _usageManager;
        private readonly DispatcherTimer _timer;
        private System.Windows.Forms.NotifyIcon _notifyIcon;
        private System.Windows.Controls.ContextMenu _trayMenu;

        private DateTime _lastNetworkUpdate = DateTime.MinValue;
        private AppSettings _settings;

        // Sparkline data queues (store last 60 points)
        private const int MaxSparklinePoints = 60;
        private Queue<float> _cpuHistory = new Queue<float>();
        private List<Queue<float>> _gpuHistories = new List<Queue<float>>();
        private Queue<float> _ramHistory = new Queue<float>();
        private Queue<float> _diskReadHistory = new Queue<float>();
        private Queue<float> _diskWriteHistory = new Queue<float>();
        private Queue<float> _uploadHistory = new Queue<float>();
        private Queue<float> _downloadHistory = new Queue<float>();

        public MainWindow()
        {
            InitializeComponent();
            _settings = SettingsManager.Load();
            _usageManager = new NetworkUsageManager();
            _networkMonitor = new NetworkMonitor();
            _networkMonitor.OnNetworkUpdate += NetworkMonitor_OnNetworkUpdate;
            _hardwareMonitor = new HardwareMonitor();
            _processMonitor = new ProcessMonitor();

            _timer = new DispatcherTimer();
            _timer.Interval = TimeSpan.FromMilliseconds(100);
            _timer.Tick += Timer_Tick;

            InitializeTrayIcon();

            Loaded += MainWindow_Loaded;

            // Ensure we save on exit
            Application.Current.Exit += (s, e) => _usageManager.ForceSave();
        }

        private void InitializeTrayIcon()
        {
            _trayMenu = new System.Windows.Controls.ContextMenu();
            
            // Populate initial Adapters placeholder
            var adaptersItem = new System.Windows.Controls.MenuItem { Header = "Adapters" };
            _trayMenu.Items.Add(adaptersItem);

            var disksItem = new System.Windows.Controls.MenuItem { Header = "Disks" };
            _trayMenu.Items.Add(disksItem);
            
            var autoStartItem = new System.Windows.Controls.MenuItem { Header = "Run at Startup", IsCheckable = true, IsChecked = AutoStartManager.IsAutoStartEnabled() };
            autoStartItem.Click += (s, e) => {
                bool newState = !autoStartItem.IsChecked;
                AutoStartManager.SetAutoStart(newState);
                autoStartItem.IsChecked = newState;
            };
            _trayMenu.Items.Add(autoStartItem);

            var speedUnitItem = new System.Windows.Controls.MenuItem { Header = "Speed Unit" };
            foreach (SpeedUnit unit in Enum.GetValues(typeof(SpeedUnit)))
            {
                var unitItem = new System.Windows.Controls.MenuItem { Header = unit.ToString(), IsCheckable = true };
                unitItem.Click += (s, e) => {
                    SelectSpeedUnit(unit);
                };
                unitItem.IsChecked = _settings.NetworkSpeedUnit == unit;
                speedUnitItem.Items.Add(unitItem);
            }
            _trayMenu.Items.Add(speedUnitItem);

            var viewItem = new System.Windows.Controls.MenuItem { Header = "View" };
            
            var cpuItem = new System.Windows.Controls.MenuItem { Header = "CPU", IsCheckable = true, IsChecked = _settings.ShowCpu };
            cpuItem.Click += (s, e) => {
                _settings.ShowCpu = cpuItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(cpuItem);

            var gpuItem = new System.Windows.Controls.MenuItem { Header = "GPU", IsCheckable = true, IsChecked = _settings.ShowGpu };
            gpuItem.Click += (s, e) => {
                _settings.ShowGpu = gpuItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(gpuItem);

            var ramItem = new System.Windows.Controls.MenuItem { Header = "RAM", IsCheckable = true, IsChecked = _settings.ShowRam };
            ramItem.Click += (s, e) => {
                _settings.ShowRam = ramItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(ramItem);

            var diskItem = new System.Windows.Controls.MenuItem { Header = "Disk", IsCheckable = true, IsChecked = _settings.ShowDisk };
            diskItem.Click += (s, e) => {
                _settings.ShowDisk = diskItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(diskItem);

            var networkItem = new System.Windows.Controls.MenuItem { Header = "Network", IsCheckable = true, IsChecked = _settings.ShowNetwork };
            networkItem.Click += (s, e) => {
                _settings.ShowNetwork = networkItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(networkItem);

            var usageItem = new System.Windows.Controls.MenuItem { Header = "Total Usage", IsCheckable = true, IsChecked = _settings.ShowUsage };
            usageItem.Click += (s, e) => {
                _settings.ShowUsage = usageItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(usageItem);

            var processItem = new System.Windows.Controls.MenuItem { Header = "Top Process", IsCheckable = true, IsChecked = _settings.ShowTopProcess };
            processItem.Click += (s, e) => {
                _settings.ShowTopProcess = processItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(processItem);

            viewItem.Items.Add(new System.Windows.Controls.Separator());

            var sparklinesItem = new System.Windows.Controls.MenuItem { Header = "Sparklines", IsCheckable = true, IsChecked = _settings.ShowSparklines };
            sparklinesItem.Click += (s, e) => {
                _settings.ShowSparklines = sparklinesItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(sparklinesItem);

            var textMetricsItem = new System.Windows.Controls.MenuItem { Header = "Text Data", IsCheckable = true, IsChecked = _settings.ShowTextMetrics };
            textMetricsItem.Click += (s, e) => {
                _settings.ShowTextMetrics = textMetricsItem.IsChecked;
                SettingsManager.Save(_settings);
                UpdateVisibility();
            };
            viewItem.Items.Add(textMetricsItem);

            _trayMenu.Items.Add(viewItem);

            var reportItem = new System.Windows.Controls.MenuItem { Header = "Usage Report" };
            reportItem.Click += (s, e) => {
                var reportWindow = new UsageReportWindow();
                reportWindow.Show();
            };
            _trayMenu.Items.Add(reportItem);

            _trayMenu.Items.Add(new System.Windows.Controls.Separator());

            var exitItem = new System.Windows.Controls.MenuItem { Header = "Exit" };
            exitItem.Click += (s, e) => {
                _notifyIcon.Visible = false;
                Application.Current.Shutdown();
            };
            _trayMenu.Items.Add(exitItem);

            _notifyIcon = new System.Windows.Forms.NotifyIcon();
            _notifyIcon.Text = "Network Monitor";
            
            try
            {
                var iconStream = Application.GetResourceStream(new Uri("pack://application:,,,/app_icon.ico"))?.Stream;
                if (iconStream != null)
                {
                    _notifyIcon.Icon = new System.Drawing.Icon(iconStream);
                }
                else
                {
                    _notifyIcon.Icon = SystemIcons.Application; 
                }
            }
            catch
            {
                _notifyIcon.Icon = SystemIcons.Application;
            }
            
            _notifyIcon.MouseUp += NotifyIcon_MouseUp;
            _notifyIcon.Visible = true;
        }

        private void NotifyIcon_MouseUp(object? sender, System.Windows.Forms.MouseEventArgs e)
        {
            if (e.Button == System.Windows.Forms.MouseButtons.Right)
            {
                var helper = new System.Windows.Interop.WindowInteropHelper(this);
                User32.SetForegroundWindow(helper.Handle);
                _trayMenu.IsOpen = true;
            }
        }

        private void SelectSpeedUnit(SpeedUnit unit)
        {
            if (_settings.NetworkSpeedUnit != unit)
            {
                _settings.NetworkSpeedUnit = unit;
                SettingsManager.Save(_settings);

                // Update checkmarks
                foreach(object itemObj in _trayMenu.Items)
                {
                    if (itemObj is System.Windows.Controls.MenuItem menuItem && menuItem.Header as string == "Speed Unit")
                    {
                        foreach (object subItemObj in menuItem.Items)
                        {
                            if (subItemObj is System.Windows.Controls.MenuItem subItem)
                            {
                                subItem.IsChecked = subItem.Header as string == unit.ToString();
                            }
                        }
                    }
                }
            }
        }

        protected override void OnSourceInitialized(EventArgs e)
        {
            base.OnSourceInitialized(e);
            GuiHelper.SetWindowStyles(this);
        }

        private void MainWindow_Loaded(object sender, RoutedEventArgs e)
        {
            PopulateAdapters();
            PopulateDisks();
            _timer.Start();
            UpdateVisibility();
            UpdatePosition();
            
            // Debug sensors
            _hardwareMonitor.Update();
            _hardwareMonitor.DebugSensors();
        }

        private void PopulateAdapters()
        {
            var adapters = NetworkAdapterManager.GetActiveAdapters();
            // _trayMenu.Items[0] is "Adapters"
            if (_trayMenu.Items[0] is System.Windows.Controls.MenuItem adaptersMenu)
            {
                adaptersMenu.Items.Clear();

                foreach (var adapter in adapters)
                {
                    var item = new System.Windows.Controls.MenuItem { Header = adapter, IsCheckable = true };
                    item.Click += (s, args) => {
                        SelectAdapter(adapter);
                    };
                    adaptersMenu.Items.Add(item);
                }

                if (adapters.Count > 0)
                {
                    string adapterToSelect = adapters.First();

                    // If we have a stored preference and it's valid, use it
                    if (!string.IsNullOrEmpty(_settings.PreferredNetworkAdapter) && adapters.Contains(_settings.PreferredNetworkAdapter))
                    {
                        adapterToSelect = _settings.PreferredNetworkAdapter;
                    }

                    SelectAdapter(adapterToSelect);
                }
            }
        }

        private void SelectAdapter(string adapter)
        {
            _networkMonitor.Initialize(adapter);
            
            // Save preference
            if (_settings.PreferredNetworkAdapter != adapter)
            {
                _settings.PreferredNetworkAdapter = adapter;
                SettingsManager.Save(_settings);
            }
            
            // Update checkmarks
            if (_trayMenu.Items[0] is System.Windows.Controls.MenuItem adaptersMenu)
            {
                foreach (object itemObj in adaptersMenu.Items)
                {
                    if (itemObj is System.Windows.Controls.MenuItem item)
                    {
                        item.IsChecked = item.Header as string == adapter;
                    }
                }
            }
        } 

        private void PopulateDisks()
        {
            var disks = _hardwareMonitor.GetAvailableDisks();
            
            if (_trayMenu.Items[1] is System.Windows.Controls.MenuItem disksMenu)
            {
                disksMenu.Items.Clear();

                foreach (var disk in disks)
                {
                    var item = new System.Windows.Controls.MenuItem { Header = disk, IsCheckable = true };
                    item.Click += (s, args) => {
                        SelectDisk(disk);
                    };
                    disksMenu.Items.Add(item);
                }

                if (disks.Count > 0)
                {
                    string diskToSelect = "All Disks";
                    if (!string.IsNullOrEmpty(_settings.PreferredDisk) && disks.Contains(_settings.PreferredDisk))
                    {
                        diskToSelect = _settings.PreferredDisk;
                    }
                    SelectDisk(diskToSelect);
                }
            }
        }

        private void SelectDisk(string disk)
        {
            if (_settings.PreferredDisk != disk)
            {
                _settings.PreferredDisk = disk;
                SettingsManager.Save(_settings);
            }
            
            if (_trayMenu.Items[1] is System.Windows.Controls.MenuItem disksMenu)
            {
                foreach (object itemObj in disksMenu.Items)
                {
                    if (itemObj is System.Windows.Controls.MenuItem item)
                    {
                        item.IsChecked = item.Header as string == disk;
                    }
                }
            }
        }

        private void Timer_Tick(object? sender, EventArgs e)
        {
            UpdatePosition();
            
            // Aggressive Z-Order enforcement
            var helper = new System.Windows.Interop.WindowInteropHelper(this);
            User32.SetWindowPos(helper.Handle, User32.HWND_TOPMOST, 0, 0, 0, 0, User32.SWP_NOMOVE | User32.SWP_NOSIZE | User32.SWP_NOACTIVATE);

            if ((DateTime.Now - _lastNetworkUpdate).TotalMilliseconds >= 1000)
            {
                _networkMonitor.Update();
                _hardwareMonitor.Update();
                UpdateHardwareStats();
                
                // Update Usage Text
                if (_settings.ShowUsage)
                {
                    var todayUsage = _usageManager.GetDailyUsage(DateTime.Now);
                    UsageUploadText.Text = $"T: ▲ {FormatBytes(todayUsage.BytesSent)}";
                    UsageDownloadText.Text = $"T: ▼ {FormatBytes(todayUsage.BytesReceived)}";
                }

                if (_settings.ShowTopProcess)
                {
                    var topIo = _processMonitor.GetTopIoProcess();
                    TopProcessText.Text = $"{topIo.Name.ToUpper()} ({topIo.IoSpeedMBs:F1} MB/s)";
                }

                _lastNetworkUpdate = DateTime.Now;
            }
        }

        private void NetworkMonitor_OnNetworkUpdate(object? sender, (float Upload, float Download) e)
        {
            // Update queue
            AddHistoryData(_uploadHistory, e.Upload);
            AddHistoryData(_downloadHistory, e.Download);

            // UI Update must happen on UI thread
            Dispatcher.Invoke(() => {
                UploadText.Text = $"▲ {FormatSpeed(e.Upload)}";
                DownloadText.Text = $"▼ {FormatSpeed(e.Download)}";
                
                if (_settings.ShowSparklines)
                {
                    float maxUp = _uploadHistory.Count > 0 ? _uploadHistory.Max() : 1;
                    if (maxUp < 1024) maxUp = 1024; // Min scale 1 KB/s
                    DrawSparkline(UploadSparkline, _uploadHistory, maxUp);

                    float maxDown = _downloadHistory.Count > 0 ? _downloadHistory.Max() : 1;
                    if (maxDown < 1024) maxDown = 1024; // Min scale 1 KB/s
                    DrawSparkline(DownloadSparkline, _downloadHistory, maxDown);
                }
            });

            // Track usage
            _usageManager.AddUsage((long)Math.Round(e.Upload), (long)Math.Round(e.Download));
        }

        private string FormatSpeed(float bytesPerSec)
        {
            float value = 0;
            string unit = "";

            switch (_settings.NetworkSpeedUnit)
            {
                case SpeedUnit.Mbps:
                    value = (bytesPerSec * 8) / 1000000f; // Megabits
                    unit = "Mbps";
                    break;
                case SpeedUnit.MBs:
                    value = bytesPerSec / 1048576f; // Megabytes (1024*1024)
                    unit = "MB/s";
                    break;
                case SpeedUnit.Kbps:
                    value = (bytesPerSec * 8) / 1000f; // Kilobits
                    unit = "Kbps";
                    break;
                case SpeedUnit.KBs:
                default:
                    value = bytesPerSec / 1024f; // Kilobytes
                    unit = "KB/s";
                    break;
            }

            return $"{value:F1} {unit}";
        }

        private string FormatBytes(long bytes)
        {
            string[] sizes = { "B", "KB", "MB", "GB", "TB" };
            double len = bytes;
            int order = 0;
            while (len >= 1024 && order < sizes.Length - 1)
            {
                order++;
                len = len / 1024;
            }
            return $"{len:0.##} {sizes[order]}";
        }

        private void UpdateHardwareStats()
        {
            // CPU
            float cpuLoad = _hardwareMonitor.GetCpuUsage();
            CpuLoadText.Text = $"CPU: {cpuLoad:F0}%";
            AddHistoryData(_cpuHistory, cpuLoad);
            if (_settings.ShowSparklines) DrawSparkline(CpuSparkline, _cpuHistory, 100);

            // RAM
            float ramLoad = _hardwareMonitor.GetRamUsage();
            RamLoadText.Text = $"RAM: {ramLoad:F0}%";
            AddHistoryData(_ramHistory, ramLoad);
            if (_settings.ShowSparklines) DrawSparkline(RamSparkline, _ramHistory, 100);

            // Disk
            var diskUsage = _hardwareMonitor.GetDiskUsage(_settings.PreferredDisk);
            DiskReadText.Text = $"R: {diskUsage.ReadSpeedMBs:F1} MB/s";
            DiskWriteText.Text = $"W: {diskUsage.WriteSpeedMBs:F1} MB/s";
            AddHistoryData(_diskReadHistory, diskUsage.ReadSpeedMBs);
            AddHistoryData(_diskWriteHistory, diskUsage.WriteSpeedMBs);
            if (_settings.ShowSparklines)
            {
                float readMax = _diskReadHistory.Count > 0 ? _diskReadHistory.Max() : 1;
                if (readMax < 10) readMax = 10;
                DrawSparkline(DiskReadSparkline, _diskReadHistory, readMax);

                float writeMax = _diskWriteHistory.Count > 0 ? _diskWriteHistory.Max() : 1;
                if (writeMax < 10) writeMax = 10;
                DrawSparkline(DiskWriteSparkline, _diskWriteHistory, writeMax);
            }

            // GPU
            var gpus = _hardwareMonitor.GetGpuStats();
            
            // Rebuild if count differs
            if (GpuTextPanel.Children.Count != gpus.Count)
            {
                GpuTextPanel.Children.Clear();
                _gpuHistories.Clear();

                foreach (var gpu in gpus)
                {
                    _gpuHistories.Add(new Queue<float>());
                    var tb = new System.Windows.Controls.TextBlock
                    {
                        Foreground = new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(0xE0, 0xE0, 0xE0)),
                        FontSize = 9,
                        FontFamily = new System.Windows.Media.FontFamily("Segoe UI"),
                        FontWeight = FontWeights.SemiBold,
                        Margin = new Thickness(0, 0, 0, 0)
                    };
                    GpuTextPanel.Children.Add(tb);
                }
            }

            float maxGpuLoad = 0;
            for (int i = 0; i < gpus.Count; i++)
            {
                if (GpuTextPanel.Children[i] is System.Windows.Controls.TextBlock tb)
                {
                    if (gpus.Count == 1)
                        tb.Text = $"GPU: {gpus[i].Usage:F0}%";
                    else
                        tb.Text = $"GPU {i+1}: {gpus[i].Usage:F0}%";

                    AddHistoryData(_gpuHistories[i], gpus[i].Usage);
                    if (gpus[i].Usage > maxGpuLoad) maxGpuLoad = gpus[i].Usage;
                }
            }
            
            if (_settings.ShowSparklines && gpus.Count > 0)
            {
                // We will plot the highest utilized GPU on the combined Sparkline for simplicity
                var highestGpuHistory = _gpuHistories.OrderByDescending(h => h.Count > 0 ? h.Last() : 0).FirstOrDefault();
                if (highestGpuHistory != null)
                {
                    DrawSparkline(GpuSparkline, highestGpuHistory, 100);
                }
            }
        }

        private void UpdateVisibility()
        {
            var textVis = _settings.ShowTextMetrics ? Visibility.Visible : Visibility.Collapsed;
            var sparkVis = _settings.ShowSparklines ? Visibility.Visible : Visibility.Collapsed;
            var contextVis = (!_settings.ShowTextMetrics && _settings.ShowSparklines) ? Visibility.Visible : Visibility.Collapsed;

            // Entire Hardware Block logic (Hide if all 3 are disabled)
            if (HardwareStackPanel != null)
                HardwareStackPanel.Visibility = (_settings.ShowCpu || _settings.ShowGpu || _settings.ShowRam) ? Visibility.Visible : Visibility.Collapsed;

            if (CpuPanel != null)
                CpuPanel.Visibility = _settings.ShowCpu ? Visibility.Visible : Visibility.Collapsed;
            if (CpuLoadText != null) CpuLoadText.Visibility = textVis;
            if (CpuContextText != null) CpuContextText.Visibility = contextVis;
            if (CpuSparklineBorder != null) CpuSparklineBorder.Visibility = sparkVis;
            
            if (GpuPanel != null)
                GpuPanel.Visibility = _settings.ShowGpu ? Visibility.Visible : Visibility.Collapsed;
            if (GpuTextPanel != null) GpuTextPanel.Visibility = textVis;
            if (GpuContextText != null) GpuContextText.Visibility = contextVis;
            if (GpuSparklineBorder != null) GpuSparklineBorder.Visibility = sparkVis;

            if (RamPanel != null)
                RamPanel.Visibility = _settings.ShowRam ? Visibility.Visible : Visibility.Collapsed;
            if (RamLoadText != null) RamLoadText.Visibility = textVis;
            if (RamContextText != null) RamContextText.Visibility = contextVis;
            if (RamSparklineBorder != null) RamSparklineBorder.Visibility = sparkVis;

            if (DiskPanel != null)
                DiskPanel.Visibility = _settings.ShowDisk ? Visibility.Visible : Visibility.Collapsed;
            if (DiskReadText != null) DiskReadText.Visibility = textVis;
            if (DiskWriteText != null) DiskWriteText.Visibility = textVis;
            if (DiskReadContextText != null) DiskReadContextText.Visibility = contextVis;
            if (DiskWriteContextText != null) DiskWriteContextText.Visibility = contextVis;
            if (DiskReadSparklineBorder != null) DiskReadSparklineBorder.Visibility = sparkVis;
            if (DiskWriteSparklineBorder != null) DiskWriteSparklineBorder.Visibility = sparkVis;

            if (ProcessPanel != null)
                ProcessPanel.Visibility = _settings.ShowTopProcess ? Visibility.Visible : Visibility.Collapsed;

            if (NetworkPanel != null)
            {
                NetworkPanel.Visibility = _settings.ShowNetwork ? Visibility.Visible : Visibility.Collapsed;
                if (UploadText != null) UploadText.Visibility = textVis;
                if (DownloadText != null) DownloadText.Visibility = textVis;
                if (UploadContextText != null) UploadContextText.Visibility = contextVis;
                if (DownloadContextText != null) DownloadContextText.Visibility = contextVis;
                if (UploadSparklineBorder != null) UploadSparklineBorder.Visibility = sparkVis;
                if (DownloadSparklineBorder != null) DownloadSparklineBorder.Visibility = sparkVis;
            }

            if (UsagePanel != null)
            {
                UsagePanel.Visibility = _settings.ShowUsage ? Visibility.Visible : Visibility.Collapsed;
            }
        }

        private void AddHistoryData(Queue<float> queue, float value)
        {
            queue.Enqueue(value);
            if (queue.Count > MaxSparklinePoints)
            {
                queue.Dequeue();
            }
        }

        private void DrawSparkline(System.Windows.Shapes.Polyline line, Queue<float> queue, float maxValue)
        {
            line.Points.Clear();
            if (queue.Count == 0 || maxValue <= 0) return;

            double width = line.Width;
            double height = line.Height;
            double stepX = width / (MaxSparklinePoints - 1);

            int i = MaxSparklinePoints - queue.Count; // Shift x if not fully populated
            
            foreach (var value in queue)
            {
                double x = i * stepX;
                double y = height - ((value / maxValue) * height);
                // Clamp Y bounds
                if (y < 0) y = 0;
                if (y > height) y = height;

                line.Points.Add(new System.Windows.Point(x, y));
                i++;
            }
        }

        private void UpdatePosition()
        {
            var trayRect = TaskbarLocator.GetTrayCoordinates();
            if (trayRect.Right == 0 && trayRect.Bottom == 0) return;

            double scaling = GetDpiScaling();
            double width = this.Width * scaling;
            double height = this.Height * scaling;

            // Position to the left of the tray
            int left = trayRect.Left - (int)width - 5; 
            int top = trayRect.Top + (trayRect.Height - (int)height) / 2;

            if (top < 0) top = 0;

            // Debug logging
            Console.WriteLine($"Positioning at: {left}, {top} (Tray: {trayRect.Left},{trayRect.Top} {trayRect.Width}x{trayRect.Height})");

            this.Left = left / scaling;
            this.Top = top / scaling;
            
            // Force Topmost
            this.Topmost = true;
            // Or use SetWindowPos to ensure it's HWND_TOPMOST
            var helper = new System.Windows.Interop.WindowInteropHelper(this);
            User32.SetWindowPos(helper.Handle, User32.HWND_TOPMOST, 0, 0, 0, 0, User32.SWP_NOMOVE | User32.SWP_NOSIZE | User32.SWP_NOACTIVATE);
        }

        private double GetDpiScaling()
        {
            PresentationSource source = PresentationSource.FromVisual(this);
            if (source != null && source.CompositionTarget != null)
            {
                return source.CompositionTarget.TransformToDevice.M11;
            }
            return 1.0;
        }
    }

    public static class GuiHelper
    {
        public static void SetWindowStyles(Window w)
        {
            var helper = new System.Windows.Interop.WindowInteropHelper(w);
            int exStyle = User32.GetWindowLong(helper.Handle, User32.GWL_EXSTYLE);
            
            // Combine ToolWindow (Hide from Alt-Tab) and Transparent (Click-through)
            User32.SetWindowLong(helper.Handle, User32.GWL_EXSTYLE, exStyle | User32.WS_EX_TOOLWINDOW | User32.WS_EX_TRANSPARENT);
        }
    }
}
