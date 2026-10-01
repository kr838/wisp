# Drag-select the first line inside the "Selection Test Target" window.
# That window has no dialogs, so it is safe to leave open.
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;using System.Runtime.InteropServices;
public class W3{
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int X,int Y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f,uint dx,uint dy,uint d,int e);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h,System.Text.StringBuilder s,int n);
  [StructLayout(LayoutKind.Sequential)] public struct RECT{public int Left,Top,Right,Bottom;}
  public const uint LD=0x0002, LU=0x0004;
}
"@
# Find the test target window by class name
$target=$null
Get-Process -ErrorAction SilentlyContinue | ForEach-Object {
  if($_ -and $_.MainWindowHandle -ne 0){
    $sb=New-Object System.Text.StringBuilder 256
    [W3]::GetClassName($_.MainWindowHandle,$sb,256)|Out-Null
    if($sb.ToString() -eq 'SaTestTargetWnd'){ $target=$_ }
  }
}
if(-not $target){ Write-Output "NO_TARGET"; exit 2 }
$h=$target.MainWindowHandle
[W3]::SetForegroundWindow($h)|Out-Null
Start-Sleep -Milliseconds 500
$r=New-Object W3+RECT
[W3]::GetWindowRect($h,[ref]$r)|Out-Null
# The edit control is inset ~10px; first text line sits near the top of the edit.
$x0=$r.Left+50; $x1=$r.Left+520; $y=$r.Top+91
[W3]::SetCursorPos($x0,$y)|Out-Null
Start-Sleep -Milliseconds 400
[W3]::mouse_event([W3]::LD,0,0,0,0)|Out-Null
Start-Sleep -Milliseconds 150
for($i=1;$i -le 20;$i++){ $x=[int]($x0+($x1-$x0)*$i/20); [W3]::SetCursorPos($x,$y)|Out-Null; Start-Sleep -Milliseconds 40 }
Start-Sleep -Milliseconds 250
[W3]::mouse_event([W3]::LU,0,0,0,0)|Out-Null
Write-Output "DRAG_DONE $x0-$x1 @ y=$y"
