using System;
using WindowsNetworkMonitor.Interop;

namespace WindowsNetworkMonitor.Services
{
    public class TaskbarLocator
    {
        public static User32.RECT GetTrayCoordinates()
        {
            IntPtr taskbarHandle = User32.FindWindow("Shell_TrayWnd", null);
            if (taskbarHandle == IntPtr.Zero) return new User32.RECT();

            IntPtr trayNotifyHandle = User32.FindWindowEx(taskbarHandle, IntPtr.Zero, "TrayNotifyWnd", null);
            if (trayNotifyHandle == IntPtr.Zero) return new User32.RECT();

            // Depending on Windows version, TrayNotifyWnd might be inside ReBarWindow32
            // For Windows 11, structure can be complex. 
            // Often Shell_TrayWnd -> Stack -> ... -> TrayNotifyWnd. 
            // Simple FindWindowEx might fail if it's nested deep.
            // For now, let's try direct child. If fails, we might need to search recursively or assume position.
            
            // Fallback for Windows 11: sometimes it's different. 
            // Let's try to get the Shell_TrayWnd rect first as a baseline.
            User32.GetWindowRect(taskbarHandle, out User32.RECT taskbarRect);
            
            if (trayNotifyHandle != IntPtr.Zero)
            {
                User32.GetWindowRect(trayNotifyHandle, out User32.RECT trayRect);
                return trayRect;
            }

            return taskbarRect;
        }

        public static User32.RECT GetTaskbarRect()
        {
            IntPtr taskbarHandle = User32.FindWindow("Shell_TrayWnd", null);
            if (taskbarHandle == IntPtr.Zero) return new User32.RECT();

            User32.GetWindowRect(taskbarHandle, out User32.RECT rect);
            return rect;
        }
    }
}
