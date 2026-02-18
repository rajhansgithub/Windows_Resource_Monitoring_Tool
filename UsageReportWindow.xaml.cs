using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using WindowsNetworkMonitor.Services;

namespace WindowsNetworkMonitor
{
    public partial class UsageReportWindow : Window
    {
        private readonly NetworkUsageManager _usageManager;

        public UsageReportWindow()
        {
            InitializeComponent();
            _usageManager = new NetworkUsageManager(); // Load data
            LoadData();
        }

        private void LoadData()
        {
            var data = _usageManager.GetUsageData();
            var today = DateTime.Now.ToString("yyyy-MM-dd");
            var currentMonth = DateTime.Now.ToString("yyyy-MM");
            var currentYear = DateTime.Now.ToString("yyyy");

            // Daily Data
            var dailyList = new List<UsageItem>();
            foreach (var kvp in data.DailyStats.OrderByDescending(x => x.Key))
            {
                dailyList.Add(new UsageItem
                {
                    Date = kvp.Key,
                    Upload = FormatBytes(kvp.Value.BytesSent),
                    Download = FormatBytes(kvp.Value.BytesReceived),
                    Total = FormatBytes(kvp.Value.BytesSent + kvp.Value.BytesReceived)
                });
            }
            DailyGrid.ItemsSource = dailyList;

            // Monthly Data
            var monthlyStats = new Dictionary<string, (long Sent, long Received)>();
            foreach (var kvp in data.DailyStats)
            {
                var monthKey = kvp.Key.Substring(0, 7); // yyyy-MM
                if (!monthlyStats.ContainsKey(monthKey)) monthlyStats[monthKey] = (0, 0);
                
                var current = monthlyStats[monthKey];
                monthlyStats[monthKey] = (current.Sent + kvp.Value.BytesSent, current.Received + kvp.Value.BytesReceived);
            }

            var monthlyList = new List<UsageItem>();
            foreach(var kvp in monthlyStats.OrderByDescending(x => x.Key))
            {
                monthlyList.Add(new UsageItem
                {
                    Date = kvp.Key,
                    Upload = FormatBytes(kvp.Value.Sent),
                    Download = FormatBytes(kvp.Value.Received),
                    Total = FormatBytes(kvp.Value.Sent + kvp.Value.Received)
                });
            }
            MonthlyGrid.ItemsSource = monthlyList;

            // Yearly Data
            var yearlyStats = new Dictionary<string, (long Sent, long Received)>();
            foreach (var kvp in data.DailyStats)
            {
                var yearKey = kvp.Key.Substring(0, 4); // yyyy
                if (!yearlyStats.ContainsKey(yearKey)) yearlyStats[yearKey] = (0, 0);

                var current = yearlyStats[yearKey];
                yearlyStats[yearKey] = (current.Sent + kvp.Value.BytesSent, current.Received + kvp.Value.BytesReceived);
            }

            var yearlyList = new List<UsageItem>();
            foreach (var kvp in yearlyStats.OrderByDescending(x => x.Key))
            {
                yearlyList.Add(new UsageItem
                {
                    Date = kvp.Key,
                    Upload = FormatBytes(kvp.Value.Sent),
                    Download = FormatBytes(kvp.Value.Received),
                    Total = FormatBytes(kvp.Value.Sent + kvp.Value.Received)
                });
            }
            YearlyGrid.ItemsSource = yearlyList;

            // Totals Text
            if (data.DailyStats.ContainsKey(today))
            {
                var t = data.DailyStats[today];
                DailyTotalText.Text = FormatBytes(t.BytesSent + t.BytesReceived);
            }

            if (monthlyStats.ContainsKey(currentMonth))
            {
                var m = monthlyStats[currentMonth];
                MonthlyTotalText.Text = FormatBytes(m.Sent + m.Received);
            }

            if (yearlyStats.ContainsKey(currentYear))
            {
                var y = yearlyStats[currentYear];
                YearlyTotalText.Text = FormatBytes(y.Sent + y.Received);
                
            }

            // All Time
            var allTime = _usageManager.GetTotalUsage();
            TotalUploadText.Text = FormatBytes(allTime.BytesSent);
            TotalDownloadText.Text = FormatBytes(allTime.BytesReceived);
            AllTimeTotalText.Text = "Total: " + FormatBytes(allTime.BytesSent + allTime.BytesReceived);
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

        public class UsageItem
        {
            public string Date { get; set; }
            public string Upload { get; set; }
            public string Download { get; set; }
            public string Total { get; set; }
        }
    }
}
