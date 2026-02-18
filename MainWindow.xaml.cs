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
        private readonly DispatcherTimer _timer;
        private System.Windows.Forms.NotifyIcon _notifyIcon;
        private System.Windows.Forms.ContextMenuStrip _trayMenu;
        private DateTime _lastNetworkUpdate = DateTime.MinValue;

        public MainWindow()
        {
            InitializeComponent();
            _networkMonitor = new NetworkMonitor();
            _networkMonitor.OnNetworkUpdate += NetworkMonitor_OnNetworkUpdate;
            _hardwareMonitor = new HardwareMonitor();

            _timer = new DispatcherTimer();
            _timer.Interval = TimeSpan.FromMilliseconds(100);
            _timer.Tick += Timer_Tick;

            InitializeTrayIcon();

            Loaded += MainWindow_Loaded;
        }

        private void InitializeTrayIcon()
        {
            _trayMenu = new System.Windows.Forms.ContextMenuStrip();
            
            // Populate initial Adapters placeholder
            var adaptersItem = new System.Windows.Forms.ToolStripMenuItem("Adapters");
            _trayMenu.Items.Add(adaptersItem);
            
            var autoStartItem = new System.Windows.Forms.ToolStripMenuItem("Run at Startup");
            autoStartItem.Checked = AutoStartManager.IsAutoStartEnabled();
            autoStartItem.Click += (s, e) => {
                bool newState = !autoStartItem.Checked;
                AutoStartManager.SetAutoStart(newState);
                autoStartItem.Checked = newState;
            };
            _trayMenu.Items.Add(autoStartItem);

            var exitItem = new System.Windows.Forms.ToolStripMenuItem("Exit", null, (s, e) => {
                _notifyIcon.Visible = false;
                Application.Current.Shutdown();
            });
            _trayMenu.Items.Add("-");
            _trayMenu.Items.Add(exitItem);

            _notifyIcon = new System.Windows.Forms.NotifyIcon();
            _notifyIcon.Text = "Network Monitor";
            // Uses system icon for now as we don't have a resource
            _notifyIcon.Icon = SystemIcons.Application; 
            _notifyIcon.ContextMenuStrip = _trayMenu;
            _notifyIcon.Visible = true;
        }

        protected override void OnSourceInitialized(EventArgs e)
        {
            base.OnSourceInitialized(e);
            GuiHelper.SetWindowStyles(this);
        }

        private void MainWindow_Loaded(object sender, RoutedEventArgs e)
        {
            PopulateAdapters();
            _timer.Start();
            UpdatePosition();
            
            // Debug sensors
            _hardwareMonitor.Update();
            _hardwareMonitor.DebugSensors();
        }

        private void PopulateAdapters()
        {
            var adapters = NetworkAdapterManager.GetActiveAdapters();
            // _trayMenu.Items[0] is "Adapters"
            if (_trayMenu.Items[0] is System.Windows.Forms.ToolStripMenuItem adaptersMenu)
            {
                adaptersMenu.DropDownItems.Clear();

                foreach (var adapter in adapters)
                {
                    var item = new System.Windows.Forms.ToolStripMenuItem(adapter, null, (s, args) => {
                        SelectAdapter(adapter);
                    });
                    adaptersMenu.DropDownItems.Add(item);
                }

                if (adapters.Count > 0)
                {
                    SelectAdapter(adapters.First());
                }
            }
        }

        private void SelectAdapter(string adapter)
        {
            _networkMonitor.Initialize(adapter);
            
            // Update checkmarks
            if (_trayMenu.Items[0] is System.Windows.Forms.ToolStripMenuItem adaptersMenu)
            {
                foreach (System.Windows.Forms.ToolStripMenuItem item in adaptersMenu.DropDownItems)
                {
                    item.Checked = item.Text == adapter;
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
                _lastNetworkUpdate = DateTime.Now;
            }
        }

        private void NetworkMonitor_OnNetworkUpdate(object? sender, (float Upload, float Download) e)
        {
            // UI Update must happen on UI thread
            Dispatcher.Invoke(() => {
                UploadText.Text = $"▲ {FormatSpeed(e.Upload)}";
                DownloadText.Text = $"▼ {FormatSpeed(e.Download)}";
            });
        }

        private string FormatSpeed(float bytesPerSec)
        {
            float mbs = bytesPerSec / (1024f * 1024f);
            return $"{mbs:F2} MB/s";
        }

        private void UpdateHardwareStats()
        {
            // CPU
            float cpuLoad = _hardwareMonitor.GetCpuUsage();
            CpuLoadText.Text = $"CPU: {cpuLoad:F0}%";

            // GPU
            var gpus = _hardwareMonitor.GetGpuStats();
            
            // Rebuild if count differs (simplest way to handle dynamic changes)
            if (GpuPanel.Children.Count != gpus.Count)
            {
                GpuPanel.Children.Clear();
                foreach (var gpu in gpus)
                {
                    var tb = new System.Windows.Controls.TextBlock
                    {
                        Foreground = new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(0xE0, 0xE0, 0xE0)),
                        FontSize = 11,
                        FontFamily = new System.Windows.Media.FontFamily("Segoe UI"),
                        FontWeight = FontWeights.SemiBold,
                        Margin = new Thickness(0, 0, 0, 0)
                    };
                    GpuPanel.Children.Add(tb);
                }
            }

            for (int i = 0; i < gpus.Count; i++)
            {
                if (GpuPanel.Children[i] is System.Windows.Controls.TextBlock tb)
                {
                    // Shorten name if possible or just show it
                    // User asked to show "GPU 1" or "GPU 2" if multiple?
                    // Or just the Name. LibreHardwareMonitor gives full name.
                    // Let's try to be concise: "GPU: N% T°C" if 1, or "Name: ..."
                    
                    if (gpus.Count == 1)
                        tb.Text = $"GPU: {gpus[i].Usage:F0}%";
                    else
                        tb.Text = $"GPU {i+1}: {gpus[i].Usage:F0}%";
                }
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
