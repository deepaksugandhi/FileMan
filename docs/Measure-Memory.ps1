param(
    [int]$ProcessId = (Get-Process fileman -ErrorAction Stop | Select-Object -First 1).Id,
    [ValidateRange(1, 60)][int]$SampleSeconds = 10
)

# Read-only: does not trim memory, inject code, or change the target process.
$ErrorActionPreference = 'Stop'
if (-not ('FileManMemoryMap' -as [type])) {
    Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class FileManMemoryMap {
    [StructLayout(LayoutKind.Sequential)]
    struct Region {
        public IntPtr BaseAddress, AllocationBase;
        public uint AllocationProtect;
        public ushort PartitionId;
        public UIntPtr RegionSize;
        public uint State, Protect, Type;
    }
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int id);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern UIntPtr VirtualQueryEx(IntPtr process, IntPtr address, out Region region, UIntPtr length);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);

    // Committed virtual regions, NOT resident RAM or allocator ownership.
    public static ulong[] Committed(int id) {
        if (IntPtr.Size != 8 || Marshal.SizeOf(typeof(Region)) != 48)
            throw new InvalidOperationException("Run this script in 64-bit PowerShell.");
        var handle = OpenProcess(0x0400, false, id); // PROCESS_QUERY_INFORMATION
        if (handle == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        var totals = new ulong[3]; // private, mapped, image
        try {
            ulong address = 0;
            Region region;
            while (VirtualQueryEx(handle, new IntPtr((long)address), out region,
                                 new UIntPtr(48)).ToUInt64() != 0) {
                var size = region.RegionSize.ToUInt64();
                if (region.State == 0x1000) {
                    var index = region.Type == 0x20000 ? 0 : region.Type == 0x40000 ? 1 : 2;
                    totals[index] += size;
                }
                var next = (ulong)region.BaseAddress.ToInt64() + size;
                if (next <= address) throw new InvalidOperationException("Invalid memory-region size.");
                address = next;
            }
            // Windows ends enumeration with ERROR_INVALID_PARAMETER above user address space.
            if (Marshal.GetLastWin32Error() != 87)
                throw new Win32Exception(Marshal.GetLastWin32Error());
            return totals;
        } finally { CloseHandle(handle); }
    }
}
'@
}

$process = Get-Process -Id $ProcessId
$map = [FileManMemoryMap]::Committed($ProcessId)
$modules = @($process.Modules | Sort-Object ModuleMemorySize -Descending | ForEach-Object {
    [pscustomobject]@{ Name = $_.ModuleName; ImageSizeMiB = [math]::Round($_.ModuleMemorySize / 1MB, 2) }
})
$cpuStart = $process.TotalProcessorTime.TotalSeconds
$timer = [Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Seconds $SampleSeconds
$process.Refresh()
[pscustomobject]@{
    Timestamp = (Get-Date).ToString('o')
    ProcessId = $ProcessId
    Executable = $process.Path
    StartTime = $process.StartTime.ToString('o')
    SampleSeconds = [math]::Round($timer.Elapsed.TotalSeconds, 2)
    WorkingSetMiB = [math]::Round($process.WorkingSet64 / 1MB, 2)
    PrivateCommitMiB = [math]::Round($process.PrivateMemorySize64 / 1MB, 2)
    PeakWorkingSetMiB = [math]::Round($process.PeakWorkingSet64 / 1MB, 2)
    CpuOneCorePercent = [math]::Round(100 * ($process.TotalProcessorTime.TotalSeconds - $cpuStart) / $timer.Elapsed.TotalSeconds, 2)
    Threads = $process.Threads.Count
    Handles = $process.HandleCount
    RegionPrivateCommittedMiB = [math]::Round($map[0] / 1MB, 2)
    RegionMappedCommittedMiB = [math]::Round($map[1] / 1MB, 2)
    RegionImageCommittedMiB = [math]::Round($map[2] / 1MB, 2)
    Modules = $modules
} | ConvertTo-Json -Depth 4
