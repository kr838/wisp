# Click the first button in the toolbar window.
# Identifies the toolbar by its current size (~275x44) among the app's visible windows.
param([int]$BtnX = 55, [int]$BtnY = 22)
Add-Type @"
using System;using System.Text;using System.Runtime.InteropServices;
public class TC{
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f,uint dx,uint dy,uint d,int e);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [StructLayout(LayoutKind.Sequential)] public struct RECT{public int L,T,R,B;}
  public const uint LD=0x0002, LU=0x0004;
}
"@
# Declare DPI awareness so SetCursorPos/GetWindowRect both use physical pixels,
# matching the selection hook's coordinate space. -4 = PER_MONITOR_AWARE_V2.
[TC]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$best = $null
$bestArea = 0
[TC]::EnumWindows({ param($h,$l)
  $p=0; [TC]::GetWindowThreadProcessId($h,[ref]$p)|Out-Null
  $proc=Get-Process -Id $p -ErrorAction SilentlyContinue
  if($proc -and $proc.ProcessName -eq 'selection-assistant' -and [TC]::IsWindowVisible($h)){
    $r=New-Object TC+RECT; [TC]::GetWindowRect($h,[ref]$r)|Out-Null
    $w=$r.R-$r.L; $ht=$r.B-$r.T
    # Toolbar: wider than tall, height 30-70, width 150-600, not the full desktop.
    if($w -ge 150 -and $w -le 700 -and $ht -ge 28 -and $ht -le 80){
      $area = $w * $ht
      if($area -gt $bestArea){ $bestArea=$area; $script:best=$r }
    }
  }
  return $true
}, [IntPtr]::Zero) | Out-Null

if(-not $best){ Write-Output "NO_TOOLBAR"; exit 2 }
$cx = $best.L + $BtnX
$cy = $best.T + $BtnY
Write-Output "TOOLBAR [$($best.L),$($best.T),$($best.R),$($best.B)] size=$($best.R-$best.L)x$($best.B-$best.T) click=$cx,$cy"
[TC]::SetCursorPos($cx,$cy)|Out-Null
Start-Sleep -Milliseconds 300
[TC]::mouse_event([TC]::LD,0,0,0,0)
Start-Sleep -Milliseconds 100
[TC]::mouse_event([TC]::LU,0,0,0,0)
Write-Output "CLICKED"
