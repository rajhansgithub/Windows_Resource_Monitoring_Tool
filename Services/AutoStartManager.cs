using Microsoft.Win32;
using System;
using System.Reflection;
using System.Windows;

namespace WindowsNetworkMonitor.Services
{
    public static class AutoStartManager
    {
        private const string RunKey = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
        private const string AppName = "WindowsNetworkMonitor";

        public static bool IsAutoStartEnabled()
        {
            using (RegistryKey? key = Registry.CurrentUser.OpenSubKey(RunKey))
            {
                return key?.GetValue(AppName) != null;
            }
        }

        public static void SetAutoStart(bool enable)
        {
            using (RegistryKey? key = Registry.CurrentUser.OpenSubKey(RunKey, true))
            {
                if (key == null) return;

                if (enable)
                {
                    // Get the executable path. 
                    // Note: For .NET Core/5+, Process.GetCurrentProcess().MainModule.FileName is usually good.
                    // Or Environment.ProcessPath
                    string? path = Environment.ProcessPath;
                    if (path != null)
                    {
                        // Add quotes to handle spaces in path
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
