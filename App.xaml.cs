using System;
using System.IO;
using System.Windows;

namespace WindowsNetworkMonitor
{
    public partial class App : System.Windows.Application
    {
        protected override void OnStartup(StartupEventArgs e)
        {
            base.OnStartup(e);

            if (WindowsNetworkMonitor.Services.AutoStartManager.EnsureInstalled())
            {
                new MainWindow().Show();
            }
        }

        public App()
        {
            AppDomain.CurrentDomain.UnhandledException += CurrentDomain_UnhandledException;
            DispatcherUnhandledException += App_DispatcherUnhandledException;
        }

        private void App_DispatcherUnhandledException(object sender, System.Windows.Threading.DispatcherUnhandledExceptionEventArgs e)
        {
            LogCrash(e.Exception);
            e.Handled = true; 
            System.Windows.MessageBox.Show($"An unexpected error occurred: {e.Exception.Message}\n\nCheck crash_log.txt for details.", "Resource Monitor Error", System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Error);
            Shutdown();
        }

        private void CurrentDomain_UnhandledException(object sender, UnhandledExceptionEventArgs e)
        {
            if (e.ExceptionObject is Exception ex)
            {
                LogCrash(ex);
                System.Windows.MessageBox.Show($"A critical error occurred: {ex.Message}\n\nCheck crash_log.txt for details.", "Resource Monitor Critical Error", System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Error);
            }
        }

        private void LogCrash(Exception ex)
        {
            try
            {
                string logFile = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "crash_log.txt");
                string logContent = $"[{DateTime.Now}] Crash Report:\n" +
                                    $"Message: {ex.Message}\n" +
                                    $"StackTrace: {ex.StackTrace}\n" +
                                    $"Source: {ex.Source}\n" +
                                    $"--------------------------------------------------\n";
                File.AppendAllText(logFile, logContent);
            }
            catch
            {
                // Failed to log
            }
        }
    }
}
