using System;
using System.Diagnostics;
using System.Threading.Tasks;

namespace WindowsNetworkMonitor.Services
{
    public class NetworkMonitor
    {
        private PerformanceCounter? _downloadCounter;
        private PerformanceCounter? _uploadCounter;
        private string? _currentInstanceName;

        public event EventHandler<(float Upload, float Download)>? OnNetworkUpdate;

        public void Initialize(string instanceName)
        {
            _currentInstanceName = instanceName;
            try
            {
                _downloadCounter = new PerformanceCounter("Network Interface", "Bytes Received/sec", instanceName);
                _uploadCounter = new PerformanceCounter("Network Interface", "Bytes Sent/sec", instanceName);
                
                // First read usually returns 0
                _downloadCounter.NextValue();
                _uploadCounter.NextValue();
            }
            catch (Exception ex)
            {
                Debug.WriteLine($"Error initializing counters: {ex.Message}");
            }
        }

        public void Update()
        {
            if (_downloadCounter == null || _uploadCounter == null) return;

            try
            {
                float download = _downloadCounter.NextValue();
                float upload = _uploadCounter.NextValue();
                OnNetworkUpdate?.Invoke(this, (upload, download));
            }
            catch
            {
                // Handle potential errors (e.g. adapter disconnected)
            }
        }
    }
}
