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
            public float Temperature;
        }

        public HardwareMonitor()
        {
            _computer = new Computer
            {
                IsCpuEnabled = true,
                IsGpuEnabled = true,
                // IsMemoryEnabled = true, // Not requested but good to have if needed
                // IsMotherboardEnabled = true,
                // IsControllerEnabled = true,
                // IsNetworkEnabled = true,
                // IsStorageEnabled = true
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

        public float GetCpuTemperature()
        {
            var cpu = _computer.Hardware.FirstOrDefault(h => h.HardwareType == HardwareType.Cpu);
            if (cpu == null) return 0;

            // Priority: "Package" -> "Average" -> "Core Max" -> Max of any temp
            var temp = cpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature && s.Name.Contains("Package"))
                       ?? cpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature && s.Name == "CPU Average")
                       ?? cpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature && s.Name == "Core Max");
            
            if (temp == null)
            {
                 var maxTemp = cpu.Sensors.Where(s => s.SensorType == SensorType.Temperature).MaxBy(s => s.Value)?.Value;
                 return maxTemp ?? 0;
            }

            return temp.Value ?? 0;
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

                // GPU Temp priority: "GPU Core" -> "GPU Hot Spot" -> Any Temp
                var temp = gpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature && s.Name == "GPU Core")?.Value
                           ?? gpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature && s.Name == "GPU Hot Spot")?.Value
                           ?? gpu.Sensors.FirstOrDefault(s => s.SensorType == SensorType.Temperature)?.Value
                           ?? 0;

                stats.Add(new GpuStats
                {
                    Name = gpu.Name,
                    Usage = usage,
                    Temperature = temp
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
