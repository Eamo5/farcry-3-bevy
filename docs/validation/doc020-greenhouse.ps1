# Repositioned vine fixture followed by ordinary look/walk/jump inputs.
# Requires converted installed assets and a paired DOC_020_B20E checkpoint.
param(
    [Parameter(Mandatory = $true)][string]$Checkpoint,
    [switch]$Doorway,
    [switch]$Interior,
    [switch]$SecondDoor,
    [string]$Output = "$env:LOCALAPPDATA\Temp\opencode\doc020-greenhouse",
    [string]$Binary = ''
)
$ErrorActionPreference = 'Stop'
if (!$Binary) { $Binary = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..\..\target\release\farcry3.exe' }
$Checkpoint = (Resolve-Path $Checkpoint).Path
$Binary = (Resolve-Path $Binary).Path
$game = [IO.Path]::ChangeExtension($Checkpoint, '.game.txt')
if (!(Test-Path $game)) { throw "Missing paired gameplay snapshot: $game" }
New-Item -ItemType Directory -Force -Path $Output | Out-Null
$Output = (Resolve-Path $Output).Path
$saved = @{}
Get-ChildItem Env: | Where-Object { $_.Name -like 'FC3_*' -or $_.Name -eq 'RUST_LOG' } | ForEach-Object {
    $saved[$_.Name] = $_.Value
    Remove-Item "Env:$($_.Name)"
}
try {
    $env:FC3_STORY = '1'
    $env:FC3_VINE_GAMEPLAY_TEST = '1'
    $env:FC3_TRAVERSAL_TEST = 'ladder'
    $env:FC3_TRAVERSAL_LADDER_ID = '2078490335079601680'
    $env:FC3_TRAVERSAL_WALK_TO = '-1880,-2460;-1870,-2460;-1870,-2440;-1865,-2430;-1880,-2422;-1890.59,-2413.37;-1892.61,-2406.09;-1894.49,-2398.20'
    if ($Doorway -or $Interior -or $SecondDoor) { $env:FC3_TRAVERSAL_WALK_TO += ';-1896.96,-2388.27' }
    if ($Interior -or $SecondDoor) { $env:FC3_TRAVERSAL_WALK_TO += ';-1898,-2387;-1897,-2382' }
    if ($SecondDoor) {
        $env:FC3_TRAVERSAL_WALK_TO += ';-1897.8,-2380.1;-1896.9,-2380.05;-1894.5,-2379.45;-1893.44,-2378.9'
        $env:FC3_TRAVERSAL_WALK_RADIUS = '0.15'
        $env:FC3_TRAVERSAL_WALK_RADIUS_FROM = '12'
        $env:FC3_TRAVERSAL_WALK_CROUCH_FROM = '12'
        $env:FC3_STORY_TRIGGER_DEBUG = '2073364939755519788'
    }
    $env:FC3_TRAVERSAL_WALK_JUMP = '1'
    $env:FC3_STORY_LOAD = $Checkpoint
    $env:RUST_LOG = 'warn,farcry3::traversal=info,farcry3::combat=info,farcry3::cinema=debug,farcry3::story=info'
    $log = Join-Path $Output 'replay.err.log'
    $image = Join-Path $Output 'greenhouse.png'
    if (Test-Path $image) { Remove-Item $image }
    $settle = if ($SecondDoor) { 115 } else { 95 }
    $process = Start-Process -FilePath $Binary -ArgumentList "--novsync --screenshot `"$image`" --settle $settle" -PassThru -Wait `
        -RedirectStandardOutput (Join-Path $Output 'replay.log') -RedirectStandardError $log
    if ($process.ExitCode -ne 0) { throw "Replay exited $($process.ExitCode); inspect $log" }
    $text = [IO.File]::ReadAllText($log) -replace '\x1b\[[0-9;]*m', ''
    $previous = -1
    $cues = @('GH_StartPOS', 'GH_POS2', 'GH_POS3')
    if ($Doorway -or $Interior -or $SecondDoor) { $cues += 'DoorOpen' }
    if ($Interior -or $SecondDoor) { $cues += 'OpenDoor' }
    if ($SecondDoor) { $cues += 'CloseDoor' }
    foreach ($cue in $cues) {
        $hits = [regex]::Matches($text, "reached $cue at")
        if ($hits.Count -ne 1 -or $hits[0].Index -le $previous) { throw "Missing, repeated or out-of-order cue: $cue" }
        $previous = $hits[0].Index
    }
    if ($text -notmatch 'post-dismount walk reached') { throw 'Physical route did not finish' }
    if ($SecondDoor -and $text -notmatch 'story trigger diagnostic 2073364939755519788: [^\r\n]*inside Some\(true\) contacts \[1\]') {
        throw 'Second-door exit trigger was not physically occupied'
    }
    if ($text -match 'player died:|walk aborted|back to the last checkpoint|\bERROR\b|\bWARN\b') { throw "Replay failed; inspect $log" }
    $health = [regex]::Matches($text, 'health ([0-9]+), towers')
    if ($health.Count -eq 0 -or $health[$health.Count - 1].Groups[1].Value -ne '100') { throw 'Final health is not 100' }
    if (!(Test-Path $image)) { throw 'Screenshot was not written' }
    Write-Output "PASS: cues $($cues -join ', '), completed physical route, health 100. Evidence: $Output"
} finally {
    Get-ChildItem Env: | Where-Object { $_.Name -like 'FC3_*' -or $_.Name -eq 'RUST_LOG' } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
    foreach ($name in $saved.Keys) { Set-Item "Env:$name" $saved[$name] }
}
