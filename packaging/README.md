# Packaging ChimeraIRC

`build-installer.ps1` builds a release and writes, to `dist\`:

* `ChimeraIRC-<version>-x64.msi` - a per-user installer (no administrator rights): the program in
  `%LocalAppData%\Programs\ChimeraIRC`, a Start menu shortcut and an entry in Settings > Apps. Upgrades replace the
  old version; removing it leaves your settings in `%AppData%\ChimeraIRC`.
* `ChimeraIRC-<version>-x64-portable.zip` - the exe, licence and a readme; no installation.

The version is the one in `Cargo.toml` and is also written into the exe's properties.

## Tools

cargo, and the WiX Toolset 5 command line. The script looks for it under `tools\` (not part of the repository):

```powershell
# one time, from the repository root (the packages are from nuget.org)
Invoke-WebRequest https://www.nuget.org/api/v2/package/wix/5.0.2 -OutFile tools\dl\wix.zip
Invoke-WebRequest https://www.nuget.org/api/v2/package/WixToolset.UI.wixext/5.0.2 -OutFile tools\dl\ui.zip
Expand-Archive tools\dl\wix.zip tools\wix; Expand-Archive tools\dl\ui.zip tools\wixui
```

(With a .NET SDK, `dotnet tool install --global wix` and `wix extension add WixToolset.UI.wixext` also work; then
pass `-WixDir` accordingly or edit the script.) `-SkipMsi` makes just the ZIP and needs no WiX.

## Signing

Unsigned files work, but Windows SmartScreen warns about them. To sign the exe and the MSI with a code-signing
certificate (a `.pfx` file from a certificate authority):

```powershell
$env:CHIMERA_SIGN_PASSWORD = Read-Host "PFX password"   # only in this session; the script never stores it
.\packaging\build-installer.ps1 -Sign -Pfx C:\keys\mycert.pfx
```

The signature is timestamped (`-TimestampUrl`, DigiCert by default; pass `""` for none), so it stays valid after the
certificate expires. A self-signed certificate signs fine but Windows will not trust it, which is only good for
testing. The signing step was tested with a throw-away self-signed certificate; no real certificate was used.

## The icon

`packaging\chimerairc-logo.webp` is the artwork. `packaging\make-icon.ps1 -Source packaging\chimerairc-logo.webp -OutIco crates\chimerairc-gui\assets\chimerairc.ico` makes the multi-size icon from it (16 to 256 pixels, the dark area outside the rounded square made transparent). Windows PowerShell with the Windows WebP codec is enough; nothing is installed.
