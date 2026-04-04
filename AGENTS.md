# AGENTS

## Windows Artifact Debug Playbook (GitHub Actions)

This repository uses `.github/workflows/build-artifacts.yml` to produce Windows artifacts (`MSI` + standalone `EXE`).

### 1. Find latest successful workflow run

```powershell
gh run list --workflow build-artifacts.yml --limit 20 `
  --json databaseId,displayTitle,headSha,conclusion,createdAt,url
```

Pick a `databaseId` where `conclusion` is `success`.

### 2. Download Windows artifact

```powershell
$runId = 23909703790 # replace
$dest = ".tmp/gha/run-$runId/windows"
gh run download $runId -n browser-port-windows -D $dest
Get-ChildItem $dest
```

Expected files:

- `browser-port-<version>-x86_64-pc-windows-msvc-unsigned.msi`
- `browser-port-<version>-x86_64-pc-windows-msvc.exe`
- `browser-port-chrome-extension-<version>.zip`

### 3. Verify standalone EXE behavior

```powershell
$exe = Resolve-Path "$dest/browser-port-0.1.0-x86_64-pc-windows-msvc.exe" # replace version if needed
try {
  $p = Start-Process -FilePath $exe.Path -PassThru -WindowStyle Hidden
  Start-Sleep -Seconds 2
  if ($p.HasExited) {
    "exited=$($p.ExitCode)"
  } else {
    "running pid=$($p.Id)"
    Stop-Process -Id $p.Id -Force
  }
} catch {
  "start-error: $($_.Exception.Message)"
}
```

### 4. Check NDI runtime candidates on test machine

```powershell
$dll = "Processing.NDI.Lib.x64.dll"
@(
  "C:\\Program Files\\NDI\\NDI 6 Runtime\\$dll",
  "C:\\Program Files\\NDI\\NDI 6 Tools\\Runtime\\$dll",
  "C:\\Program Files\\NDI\\NDI 6 Tools\\Router\\$dll",
  "C:\\Program Files\\NDI\\NDI 6 Tools\\Remote\\$dll",
  "C:\\Program Files\\NDI\\NDI 5 Runtime\\v5\\$dll"
) | ForEach-Object { "{0} => {1}" -f $_, (Test-Path $_) }
```

If none exists, NDI should remain disabled and `browser-port.exe` must still launch.

### 5. Inspect PE import/delay-load status (optional)

```powershell
$objdump = (Get-Command llvm-objdump.exe -ErrorAction Stop).Source
& $objdump -p $exe.Path | Select-String -Pattern "Import Directory|Delay Import Directory|Processing.NDI.Lib.x64.dll"
```

Target state:

- `Processing.NDI.Lib.x64.dll` appears in delay-load related section.
- Standalone launch does not fail even when NDI runtime is absent.

### 6. MSI UI check (manual)

Run the MSI and validate:

- Installer payload includes only `browser-port.exe`.
- `PATH` feature is not shown.
- `Start Menu Shortcut` feature is shown as optional.
