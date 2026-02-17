using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;

namespace WindowsNetworkMonitor.Services
{
    public class NetworkAdapterManager
    {
        public static List<string> GetActiveAdapters()
        {
            try
            {
                PerformanceCounterCategory category = new PerformanceCounterCategory("Network Interface");
                string[] instanceNames = category.GetInstanceNames();
                return instanceNames.OrderBy(n => n).ToList();
            }
            catch
            {
                return new List<string>();
            
            }
        }
    }
}
