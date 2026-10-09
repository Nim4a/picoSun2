param([string]$k = "HKCU:\Software\Classes")
@(
  "picosun2.photo","picosun2.raw","picosun2.hdr","picosun2.anim",
  "picoSun.photo","picoSun.raw","picoSun.hdr","picoSun.anim"
) | ForEach-Object {
  $sub = "$k\$_\shell\open\command"
  $v = (Get-ItemProperty -Name "(default)" -Path $sub -ErrorAction SilentlyContinue)."(default)"
  if ($v) { Write-Output "$_ => $v" }
}
