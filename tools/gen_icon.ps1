# Generates the DeepSeek Harness whale icon (PNGs + ICO) from the DSH favicon.svg.
# Uses WPF (System.Windows.Media) to parse the SVG path and rasterize it.
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName PresentationFramework
Add-Type -AssemblyName WindowsBase
Add-Type -AssemblyName System.Drawing

$svgPath = 'C:\Users\A\AppData\Local\npm-cache\_npx\1e7f6d9597241db0\node_modules\@deepseek-ai\dsh-web-frontend\dist\favicon.svg'
$projDir = Split-Path -Parent $PSScriptRoot
$assetsDir = Join-Path $projDir 'assets'
New-Item -ItemType Directory -Force -Path $assetsDir | Out-Null

$svg = Get-Content $svgPath -Raw
$m = [regex]::Match($svg, '(?:^|\s)d="([^"]+)"')
if (-not $m.Success) { throw 'SVG path data not found' }
$d = $m.Groups[1].Value

$geo = [System.Windows.Media.Geometry]::Parse($d)
$b = $geo.Bounds
Write-Output ("Geometry bounds: x={0} y={1} w={2} h={3}" -f $b.X, $b.Y, $b.Width, $b.Height)

# The favicon viewBox is 50x50; map it 1:1 onto the target pixel grid.
$viewBox = 50.0
$sizes = @(16, 24, 32, 48, 64, 128, 256, 512)
$pngFiles = @{}

foreach ($size in $sizes) {
    $scale = $size / $viewBox
    $dv = New-Object System.Windows.Media.DrawingVisual
    $dc = $dv.RenderOpen()
    $dc.PushTransform((New-Object System.Windows.Media.ScaleTransform($scale, $scale)))
    $dc.DrawGeometry([System.Windows.Media.Brushes]::Black, $null, $geo)
    $dc.Pop()
    $dc.Close()

    $rtb = New-Object System.Windows.Media.Imaging.RenderTargetBitmap($size, $size, 96, 96, [System.Windows.Media.PixelFormats]::Pbgra32)
    $rtb.Render($dv)

    $enc = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $enc.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($rtb))
    $file = Join-Path $assetsDir "whale_$size.png"
    $fs = [System.IO.File]::Create($file)
    $enc.Save($fs)
    $fs.Close()
    $pngFiles[$size] = $file
    Write-Output "wrote $file"
}

# --- Build a multi-resolution ICO (PNG-compressed entries) ---
$icoSizes = @(16, 24, 32, 48, 64, 128, 256)
$icoPath = Join-Path $assetsDir 'dsh-whale.ico'
$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ms)

# ICONDIR
$bw.Write([uint16]0)          # reserved
$bw.Write([uint16]1)          # type: icon
$bw.Write([uint16]$icoSizes.Count)

$offset = 6 + 16 * $icoSizes.Count
$blobs = @()
foreach ($size in $icoSizes) {
    $blob = [System.IO.File]::ReadAllBytes($pngFiles[$size])
    $blobs += , $blob
    $w = if ($size -ge 256) { 0 } else { $size }
    $bw.Write([byte]$w)        # width (0 => 256)
    $bw.Write([byte]$w)        # height
    $bw.Write([byte]0)         # color count
    $bw.Write([byte]0)         # reserved
    $bw.Write([uint16]1)       # planes
    $bw.Write([uint16]32)      # bit count
    $bw.Write([uint32]$blob.Length)
    $bw.Write([uint32]$offset)
    $offset += $blob.Length
}
foreach ($blob in $blobs) { $bw.Write($blob) }
$bw.Flush()
[System.IO.File]::WriteAllBytes($icoPath, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()
Write-Output "wrote $icoPath ($([System.IO.File]::ReadAllBytes($icoPath).Length) bytes)"
