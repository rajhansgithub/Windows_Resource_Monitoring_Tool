using Microsoft.Win32;
using System;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Windows;

namespace WindowsNetworkMonitor.Services
{
    public static class AutoStartManager
    {
        private const string RunKey = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
        private const string AppName = "WindowsNetworkMonitor";

        public static bool EnsureInstalled()
        {
            try
            {
                string? currentPath = Environment.ProcessPath;
                if (string.IsNullOrEmpty(currentPath)) return true;

                // Define the target installation path in LocalAppData
                string localAppData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
                string installDir = Path.Combine(localAppData, "WindowsNetworkMonitor");
                string targetPath = Path.Combine(installDir, "WindowsNetworkMonitor.exe");
                
                // Check if we are already running from the install location
                if (string.Equals(currentPath, targetPath, StringComparison.OrdinalIgnoreCase))
                {
                    // Ensure auto-start is enabled just in case
                    SetAutoStart(true);
                    return true;
                }

                // Ask the user if they want to install
                var result = System.Windows.MessageBox.Show(
                    "This application needs to be installed to run automatically on startup.\n\n" +
                    "Do you want to install it to your user profile now?",
                    "Install Windows Network Monitor",
                    MessageBoxButton.YesNo,
                    MessageBoxImage.Question);

                if (result == MessageBoxResult.Yes)
                {
                    // Create directory
                    if (!Directory.Exists(installDir))
                    {
                        Directory.CreateDirectory(installDir);
                    }

                    // Copy file
                    File.Copy(currentPath, targetPath, true);

                    // Set auto-start for the NEW location
                    SetAutoStart(true, targetPath);

                    // Start the installed instance
                    Process.Start(targetPath);

                    // Close the current instance
                    System.Windows.Application.Current.Shutdown();
                    return false;
                }
            }
            catch (Exception ex)
            {
                System.Windows.MessageBox.Show($"Failed to install application: {ex.Message}", "Installation Error", MessageBoxButton.OK, MessageBoxImage.Error);
            }

            return true;
        }

        public static bool IsAutoStartEnabled()
        {
            using (RegistryKey? key = Registry.CurrentUser.OpenSubKey(RunKey))
            {
                return key?.GetValue(AppName) != null;
            }
        }

        public static void SetAutoStart(bool enable, string? specificPath = null)
        {
            using (RegistryKey? key = Registry.CurrentUser.OpenSubKey(RunKey, true))
            {
                if (key == null) return;

                if (enable)
                {
                    string? path = specificPath ?? Environment.ProcessPath;
                    if (path != null)
                    {
                        key.SetValue(AppName, $"\"{path}\"");
                    }
                }
                else
                {
                    key.DeleteValue(AppName, false);
                }
            }
        }
    }
}
