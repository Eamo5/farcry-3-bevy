# Repositioned vine fixture followed by ordinary look/walk/jump inputs.
# Requires converted installed assets and a paired DOC_020_B20E checkpoint.
param(
    [Parameter(Mandatory = $true)][string]$Checkpoint,
    [switch]$Doorway,
    [switch]$Interior,
    [switch]$SecondDoor,
    [switch]$DarkRoom,
    # On from the dark room: the mushroom, out of the cave, up the cliffs by
    # the boulder's authored ledge and the two vine ladders, and back to the
    # doctor and Daisy (B30, B40, B50C, B60C).
    [switch]$Cliffs,
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
    if ($Cliffs) { $DarkRoom = $true }
    if ($DarkRoom) { $SecondDoor = $true }
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
    if ($DarkRoom) {
        $env:FC3_TRAVERSAL_WALK_TO += ';-1887.53,-2377.47'
        $env:FC3_TRAVERSAL_WALK_CROUCH_UNTIL = '15'
        $env:FC3_STORY_TRIGGER_DEBUG = '2073611371909540357'
    }
    if ($Cliffs) {
        # The mushroom, picked up where it grows by the fall.
        $env:FC3_TRAVERSAL_WALK_AFTER = '-1930,-2533;-1936.5,-2531.5;-1937.3,-2530.5'
        $env:FC3_TRAVERSAL_INTERACT_AT = '-1937.34,124.0,-2529.82'
        # Legs (`|`): out of the cave; to the pit; down into it by the
        # navigation mesh; up the boulder's ledge, the slope, both ladders
        # and onto the cliff top (keeping to x -1986 past a crease on the
        # shelf); by the mesh to the doctor's triggers.
        $env:FC3_TRAVERSAL_WALK_THEN = @(
            '-1938,-2537;-1946,-2540;-1955,-2543;-1962,-2548;-1966,-2551;-1969.5,-2552;-1973,-2552;-1977,-2552',
            '-1980,-2553;-1982.5,-2554;-1990,-2540',
            'nav:-1988.8,124.0,-2511.2',
            ('-1988.8,-2511.2;-1992.5,-2510.6;-1994,-2506;-1993.5,-2500;-1992,-2492;-1990.5,-2484;-1989.5,-2477;-1987.5,-2472.3;-1985.4,-2469.3;' +
                '-1986.2,-2466;-1986.2,-2458;-1986.2,-2451;-1987.3,-2449.3;-1988,-2446.3;-1994,-2445;-1997.5,-2441;-1999.5,-2435;-1998.5,-2430;' +
                '-1996,-2427.5;-1994.8,-2425.8;-1991.6,-2424.0;-1985,-2421;-1975,-2415'),
            'nav:-1931,188,-2375',
            # Off the porch's railed deck and into the doctor's house by
            # the mesh; out of its south room by a narrow doorway (points
            # `p` are reached precisely), to the foot of its stair, and up
            # it (north, then west) to Daisy upstairs.
            'nav:-1926,187.3,-2392',
            '-1925.0,-2394.2p;-1924.5,-2394.0p;-1924.5,-2393.3p;-1923.6,-2391.8;-1922.85,-2391.7p;-1922.85,-2389.5;-1923.5,-2389.0;-1924.3,-2388.9'
        ) -join '|'
        $env:FC3_STORY_TRIGGER_DEBUG = '2075710825217158732'
    }
    $env:FC3_TRAVERSAL_WALK_JUMP = '1'
    $env:FC3_STORY_LOAD = $Checkpoint
    $env:RUST_LOG = 'warn,farcry3::traversal=info,farcry3::combat=info,farcry3::cinema=debug,farcry3::story=info'
    if ($Cliffs) { $env:RUST_LOG += ',farcry3::player=info' }
    $log = Join-Path $Output 'replay.err.log'
    $image = Join-Path $Output 'greenhouse.png'
    if (Test-Path $image) { Remove-Item $image }
    $settle = if ($Cliffs) { 900 } elseif ($DarkRoom) { 125 } elseif ($SecondDoor) { 115 } else { 95 }
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
    # (The dark room's rock bridge gives way under him as he steps on it,
    # before the route's last point; the beat completing proves the drop.)
    if (!$DarkRoom -and $text -notmatch 'post-dismount walk reached') { throw 'Physical route did not finish' }
    if ($SecondDoor -and !$DarkRoom -and $text -notmatch 'story trigger diagnostic 2073364939755519788: [^\r\n]*inside Some\(true\) contacts \[1\]') {
        throw 'Second-door exit trigger was not physically occupied'
    }
    if ($DarkRoom -and $text -notmatch 'story: DOC_020_B20E Completed') {
        throw 'The dark room did not drop the player and complete the beat'
    }
    if ($Cliffs) {
        $previous = -1
        foreach ($cue in @('DOC_020_B30 Completed', 'climb: the authored ledge', 'DOC_020_B40 Completed', 'DOC_020_B50C Completed', 'DOC_020_B60C Completed')) {
            $at = $text.IndexOf($cue)
            if ($at -le $previous) { throw "Missing or out-of-order: $cue" }
            $previous = $at
        }
    }
    if ($text -match 'player died:|walk aborted|back to the last checkpoint|\bERROR\b|\bWARN\b') { throw "Replay failed; inspect $log" }
    $health = [regex]::Matches($text, 'health ([0-9]+), towers')
    if ($health.Count -eq 0 -or $health[$health.Count - 1].Groups[1].Value -ne '100') { throw 'Final health is not 100' }
    if (!(Test-Path $image)) { throw 'Screenshot was not written' }
    $beats = if ($Cliffs) { ', beats B30, B40, B50C, B60C' } else { '' }
    Write-Output "PASS: cues $($cues -join ', ')$beats, completed physical route, health 100. Evidence: $Output"
} finally {
    Get-ChildItem Env: | Where-Object { $_.Name -like 'FC3_*' -or $_.Name -eq 'RUST_LOG' } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
    foreach ($name in $saved.Keys) { Set-Item "Env:$name" $saved[$name] }
}
