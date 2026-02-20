using System;
using System.Collections.Generic;
using System.Linq;
using LibreHardwareMonitor.Hardware;

namespace WindowsNetworkMonitor.Services
{
    public class HardwareMonitor : IDisposable
    {
        private readonly Computer _computer;
        private readonly IVisitor _updateVisitor = new UpdateVisitor();

        public struct GpuStats
        {
            public string Name;
            public float Usage;
        }

        public HardwareMonitor()
        {
            _computer = new Computer
            {
                IsCpuEnabled = true,
                IsGpuEnabled = true,
                IsMemoryEnabled = true,
                IsStorageEnabled = true
            };

            try
            {
                _computer.Open();
            }
            catch (Exception)
            {
                // Likely due to missing Admin privileges
            }
        }

        public void Update()
        {
            try
            {
                _computer.Accept(_updateVisitor);
            }
            catch { }
        }

        public void DebugSensors()
        {
            Console.WriteLine("=== Detected Hardware & Sensors ===");
            foreach (var hardware in _computer.Hardware)
            {
                Console.WriteLine($"Hardware: {hardware.Name} ({hardware.HardwareType})");
                foreach (var sensor in hardware.Sensors)
                {
                    Console.WriteLine($"  - Sensor: {sensor.Name}, Type: {sensor.SensorType}, Value: {sensor.Value}");
                }
            }
            Console.WriteLine("===================================");
        }

        public float GetCpuUsage()
        {
            var cpu = _computer.Hardware.FirstOrDefault(h => h.HardwareType == HardwareType.Cpu);
            if (cpu == null) return 0;

            // Priority: "CPU Total" -> Average of cores -> Max of cores
            var load = cpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Load && s.Name == "CPU Total")
                       ?? cpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Load && s.Name == "CPU Average");

            if (load == null)
            {
                return cpu.Sensors.Where(s => s.SensorType == SensorType.Load).MaxBy(s => s.Value)?.Value ?? 0;
            }

            return load.Value ?? 0;
        }

        public float GetRamUsage()
        {
            var memory = _computer.Hardware.FirstOrDefault(h => h.HardwareType == HardwareType.Memory);
            if (memory == null) return 0;

            var load = memory.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Load && s.Name == "Memory");
            return load?.Value ?? 0;
        }

        public List<string> GetAvailableDisks()
        {
            var disks = new List<string>();
            var storageDevices = _computer.Hardware.Where(h => h.HardwareType == HardwareType.Storage);
            
            foreach (var hardware in storageDevices)
            {
                disks.Add(hardware.Name);
            }
            return disks;
        }

        public (float ReadSpeedMBs, float WriteSpeedMBs) GetDiskUsage(string selectedDisk = null)
        {
            var storageDevices = _computer.Hardware.Where(h => h.HardwareType == HardwareType.Storage);
            
            if (!string.IsNullOrEmpty(selectedDisk) && selectedDisk != "All Disks")
            {
                storageDevices = storageDevices.Where(h => h.Name == selectedDisk);
            }

            float totalRead = 0;
            float totalWrite = 0;

            foreach (var disk in storageDevices)
            {
                // FreeHardwareMonitor Storage usually has Throughput sensors in Bytes/sec
                var readSensor = disk.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Throughput && s.Name.Contains("Read"));
                var writeSensor = disk.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Throughput && s.Name.Contains("Write"));

                // Sometimes the name is "Read Rate" or just "Read"
                totalRead += readSensor?.Value ?? 0;
                totalWrite += writeSensor?.Value ?? 0;
            }

            // Return MB/s (Throughput is usually reported in Bytes/s by LibreHardwareMonitor)
            return (totalRead / (1024 * 1024), totalWrite / (1024 * 1024));
        }

        public List<GpuStats> GetGpuStats()
        {
            var gpus = _computer.Hardware.Where(h => h.HardwareType == HardwareType.GpuNvidia || h.HardwareType == HardwareType.GpuAmd || h.HardwareType == HardwareType.GpuIntel).ToList();
            var stats = new List<GpuStats>();

            foreach (var gpu in gpus)
            {
                // GPU Load priority: "GPU Core" -> Any Load
                var usage = gpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Load && s.Name == "GPU Core")?.Value
                            ?? gpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Load)?.Value 
                            ?? 0;

                stats.Add(new GpuStats
                {
                    Name = gpu.Name,
                    Usage = usage
                });
            }

            return stats;
        }

        public void Dispose()
        {
            _computer.Close();
        }

        private class UpdateVisitor : IVisitor
        {
            public void VisitComputer(IComputer computer)
            {
                computer.Traverse(this);
            }

            public void VisitHardware(IHardware hardware)
            {
                hardware.Update();
                foreach (var subHardware in hardware.SubHardware) subHardware.Accept(this);
            }

            public void VisitSensor(ISensor sensor) { }
            public void VisitParameter(IParameter parameter) { }
        }
    }
}
