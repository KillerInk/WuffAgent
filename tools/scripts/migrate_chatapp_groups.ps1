# Migrate ChatApp's flat fields to embedded group structs.
#
# Renames `self.<field>` (and `app.<field>` in sessions_actions.rs, where `app`
# is a `&mut ChatApp` parameter) to `self.<group>.<field>` — but ONLY inside
# `impl ... ChatApp` blocks (so `self.<same name>` on other types is untouched).
#
# Scope detection: a char-level scan tracking string/char/line-comment state
# (strings may span lines and contain `//`, braces, etc.).
#
# Idempotent: after the first run there is no flat `self.<field>` left to match.

$ErrorActionPreference = 'Stop'

$map = @{
    # core
    'config' = 'core'; 'server' = 'core'; 'connection' = 'core'
    'last_synced_base_url' = 'core'; 'tool_manager' = 'core'
    'agent_engine' = 'core'; 'memory_manager' = 'core'
    'memory_runtime' = 'core'; 'mcp_manager' = 'core'
    # sessions
    'session_store' = 'sessions'; 'selected_session_id' = 'sessions'
    'sub_session_tabs' = 'sessions'; 'active_tab' = 'sessions'
    'pending_images' = 'sessions'
    # dialogs
    'show_settings' = 'dialogs'; 'settings_dialog' = 'dialogs'
    'presets_dialog' = 'dialogs'; 'show_agent_config' = 'dialogs'
    'agent_config_dialog' = 'dialogs'; 'memory_panel' = 'dialogs'
    'usage_panel' = 'dialogs'; 'mcp_panel' = 'dialogs'
    'improvements_panel' = 'dialogs'
    # relay
    'pending_tx' = 'relay'; 'pending_rx' = 'relay'
    # remote
    'remote_n_ctx' = 'remote'; 'remote_n_ctx_handle' = 'remote'
    'remote_n_ctx_arc' = 'remote'
    # display
    'display_snapshot' = 'display'; 'snapshot_session' = 'display'
    'snapshot_len' = 'display'; 'display_dirty' = 'display'
    'status' = 'display'
    # restart
    'pending_restart' = 'restart'; 'pending_auto_resume' = 'restart'
    'auto_resume_reason' = 'restart'
}
# Longest names first so alternation can't match a prefix of a longer name.
$names = ($map.Keys | Sort-Object { $_.Length } -Descending) -join '|'
$rx = [regex]::new('\b(self|app)\.(' + $names + ')(?![A-Za-z0-9_])')

$filesChanged = 0
$totalRenames = 0
Get-ChildItem wuffagent-egui/src -Recurse -Filter *.rs | ForEach-Object {
    $path = $_.FullName
    $text = [System.IO.File]::ReadAllText($path)
    $chars = $text.ToCharArray()
    $n = $chars.Length

    # 1) impl-line start offsets (single-line impl headers only).
    $implOffsets = New-Object System.Collections.Generic.List[int]
    $offset = 0
    foreach ($rawLine in $text.Split("`n")) {
        if ($rawLine -match '^impl\s.*ChatApp\s*\{\s*$') { $implOffsets.Add($offset) }
        $offset += $rawLine.Length + 1
    }
    if ($implOffsets.Count -eq 0) { return }

    # 2) char scan for impl-scope ranges (string/char/comment aware).
    $ranges = New-Object System.Collections.Generic.List[object]
    $implPtr = 0; $inString = $false; $inChar = $false; $inComment = $false
    $scopeStart = -1; $depth = 0; $pendingImpl = -1
    for ($i = 0; $i -lt $n; $i++) {
        $c = $chars[$i]
        if ($implPtr -lt $implOffsets.Count -and $i -eq $implOffsets[$implPtr]) {
            if ($scopeStart -lt 0) { $pendingImpl = $i }
            $implPtr++
        }
        if ($inComment) { if ($c -eq "`n") { $inComment = $false } ; continue }
        if ($inString) {
            if ($c -eq '\') { $i++ }
            elseif ($c -eq '"') { $inString = $false }
            continue
        }
        if ($inChar) {
            if ($c -eq '\') { $i++ }
            elseif ($c -eq "'") { $inChar = $false }
            continue
        }
        if ($c -eq '"') { $inString = $true }
        elseif ($c -eq "'") { $inChar = $true }
        elseif ($c -eq '/' -and $i + 1 -lt $n -and $chars[$i + 1] -eq '/') { $inComment = $true; $i++ }
        elseif ($c -eq '{') {
            if ($scopeStart -lt 0 -and $pendingImpl -ge 0) { $scopeStart = $i; $depth = 1 }
            elseif ($scopeStart -ge 0) { $depth++ }
        }
        elseif ($c -eq '}' -and $scopeStart -ge 0) {
            $depth--
            if ($depth -eq 0) { $ranges.Add(@($scopeStart, $i + 1)); $scopeStart = -1 }
        }
        elseif ($c -eq "`n") { $pendingImpl = -1 }
    }
    if ($ranges.Count -eq 0) { return }

    # 3) apply the rename inside each scope (reverse order keeps offsets valid).
    $newText = $text
    for ($r = $ranges.Count - 1; $r -ge 0; $r--) {
        $start = $ranges[$r][0]; $end = $ranges[$r][1]
        $sub = $newText.Substring($start, $end - $start)
        $rep = $rx.Replace($sub, {
            param($m)
            $recv = $m.Groups[1].Value
            $field = $m.Groups[2].Value
            $script:totalRenames += 1
            return ($recv + '.' + $script:map[$field] + '.' + $field)
        })
        $newText = $newText.Substring(0, $start) + $rep + $newText.Substring($end)
    }
    if ($newText -cne $text) {
        [System.IO.File]::WriteAllText($path, $newText, (New-Object System.Text.UTF8Encoding($false)))
        Write-Output ('changed: ' + $path.Replace((Get-Location).Path + '\', '') + '  (scopes: ' + $ranges.Count + ')')
        $filesChanged++
    }
}
Write-Output ('files changed: ' + $filesChanged + ', renames: ' + $totalRenames)
