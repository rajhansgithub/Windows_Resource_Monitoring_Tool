using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Linq;

namespace WindowsNetworkMonitor.Services
{
    public class ProcessMonitor
    {
        [StructLayout(LayoutKind.Sequential)]
        private struct IO_COUNTERS
        {
            public ulong ReadOperationCount;
            public ulong WriteOperationCount;
            public ulong OtherOperationCount;
            public ulong ReadTransferCount;
            public ulong WriteTransferCount;
            public ulong OtherTransferCount;
        }

        [DllImport("kernel32.dll")]
        private static extern bool GetProcessIoCounters(IntPtr hProcess, out IO_COUNTERS lpIoCounters);

        private Dictionary<int, ulong> _previousTotalIo = new Dictionary<int, ulong>();
        private DateTime _lastUpdate = DateTime.MinValue;

        public (string Name, float IoSpeedMBs) GetTopIoProcess()
        {
            DateTime now = DateTime.Now;
            if (_lastUpdate == DateTime.MinValue)
            {
                _lastUpdate = now;
                UpdateCounters();
                return ("-", 0f);
            }

            double deltaSeconds = (now - _lastUpdate).TotalSeconds;
            if (deltaSeconds <= 0) deltaSeconds = 1;

            var currentIo = UpdateCounters();
            
            string topProcessName = "-";
            ulong maxIoDelta = 0;

            foreach (var kvp in currentIo)
            {
                if (_previousTotalIo.TryGetValue(kvp.Key, out ulong prevIo))
                {
                    if (kvp.Value > prevIo)
                    {
                        ulong delta = kvp.Value - prevIo;
                        if (delta > maxIoDelta)
                        {
                            maxIoDelta = delta;
                            try
                            {
                                var p = Process.GetProcessById(kvp.Key);
                                topProcessName = p.ProcessName;
                            }
                            catch
                            {
                                // Process might have exited
                            }
                        }
                    }
                }
            }

            _previousTotalIo = currentIo;
            _lastUpdate = now;

            float speedMBs = (float)(maxIoDelta / (1024.0 * 1024.0 * deltaSeconds));
            return (topProcessName, speedMBs);
        }

        private Dictionary<int, ulong> UpdateCounters()
        {
            var dict = new Dictionary<int, ulong>();
            var processes = Process.GetProcesses();

            foreach (var p in processes)
            {
                try
                {
                    if (GetProcessIoCounters(p.Handle, out IO_COUNTERS ioCounters))
                    {
                        ulong totalBytes = ioCounters.ReadTransferCount + ioCounters.WriteTransferCount + ioCounters.OtherTransferCount;
                        dict[p.Id] = totalBytes;
                    }
                }
                catch
                {
                    // Ignore processes we can't access (Access Denied for System/Idle)
                }
                finally
                {
                    p.Dispose();
                }
            }
            return dict;
        }
    }
}
