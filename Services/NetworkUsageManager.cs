using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;

namespace WindowsNetworkMonitor.Services
{
    public class NetworkUsageData
    {
        // Key: "yyyy-MM-dd"
        public Dictionary<string, DailyUsage> DailyStats { get; set; } = new Dictionary<string, DailyUsage>();
    }

    public class DailyUsage
    {
        public long BytesSent { get; set; } = 0;
        public long BytesReceived { get; set; } = 0;
    }

    public class NetworkUsageManager
    {
        private static readonly string DataFolder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "WindowsNetworkMonitor");
        private static readonly string DataFile = Path.Combine(DataFolder, "network_usage.json");
        
        private NetworkUsageData _data;
        private DateTime _lastSaveTime;

        public NetworkUsageManager()
        {
            _data = Load();
            _lastSaveTime = DateTime.Now;
        }

        public NetworkUsageData GetUsageData()
        {
            return _data;
        }

        public void AddUsage(long bytesSent, long bytesReceived)
        {
            string today = DateTime.Now.ToString("yyyy-MM-dd");

            if (!_data.DailyStats.ContainsKey(today))
            {
                _data.DailyStats[today] = new DailyUsage();
            }

            _data.DailyStats[today].BytesSent += bytesSent;
            _data.DailyStats[today].BytesReceived += bytesReceived;

            // Save periodically (e.g., every 1 minute or on critical thresholds)
            // For now, we'll save on every update to keep it simple, but in production we might want to throttle
            // However, file writes every second might be too much. Let's throttle to every 30 seconds.
            if ((DateTime.Now - _lastSaveTime).TotalSeconds >= 30)
            {
                Save();
                _lastSaveTime = DateTime.Now;
            }
        }

        public void ForceSave()
        {
            Save();
        }

        private NetworkUsageData Load()
        {
            if (!File.Exists(DataFile))
            {
                return new NetworkUsageData();
            }

            try
            {
                string json = File.ReadAllText(DataFile);
                return JsonSerializer.Deserialize<NetworkUsageData>(json) ?? new NetworkUsageData();
            }
            catch
            {
                return new NetworkUsageData();
            }
        }

        private void Save()
        {
            try
            {
                if (!Directory.Exists(DataFolder))
                {
                    Directory.CreateDirectory(DataFolder);
                }

                string json = JsonSerializer.Serialize(_data, new JsonSerializerOptions { WriteIndented = true });
                File.WriteAllText(DataFile, json);
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"Failed to save network usage: {ex.Message}");
            }
        }

        // Helper methods for reporting
        public DailyUsage GetDailyUsage(DateTime date)
        {
            string dateStr = date.ToString("yyyy-MM-dd");
            if (_data.DailyStats.ContainsKey(dateStr))
            {
                return _data.DailyStats[dateStr];
            }
            return new DailyUsage();
        }

        public DailyUsage GetTotalUsage()
        {
            long totalSent = 0;
            long totalReceived = 0;

            foreach (var day in _data.DailyStats.Values)
            {
                totalSent += day.BytesSent;
                totalReceived += day.BytesReceived;
            }

            return new DailyUsage { BytesSent = totalSent, BytesReceived = totalReceived };
        }

        public DailyUsage GetMonthlyUsage(int year, int month)
        {
             long totalSent = 0;
            long totalReceived = 0;
            string prefix = $"{year}-{month:D2}";

            foreach (var kvp in _data.DailyStats)
            {
                if (kvp.Key.StartsWith(prefix))
                {
                    totalSent += kvp.Value.BytesSent;
                    totalReceived += kvp.Value.BytesReceived;
                }
            }

            return new DailyUsage { BytesSent = totalSent, BytesReceived = totalReceived };
        }

        public DailyUsage GetYearlyUsage(int year)
        {
             long totalSent = 0;
            long totalReceived = 0;
             string prefix = $"{year}-";

            foreach (var kvp in _data.DailyStats)
            {
                 if (kvp.Key.StartsWith(prefix))
                {
                    totalSent += kvp.Value.BytesSent;
                    totalReceived += kvp.Value.BytesReceived;
                }
            }

            return new DailyUsage { BytesSent = totalSent, BytesReceived = totalReceived };
        }
    }
}
