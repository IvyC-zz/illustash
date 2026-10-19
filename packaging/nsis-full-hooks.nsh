; Windows full installer hooks (#37). An older full install had one copy of the worker per
; kind (compute\bin\illustash-<kind>.exe) and a second resources\ next to compute\. NSIS
; overwrites files but never removes ones the new package no longer ships, and the new
; uninstaller does not know them, so remove them before copying the new layout.
!include "${__FILEDIR__}\nsis-hooks.nsh"
!macro NSIS_HOOK_PREINSTALL
  Delete "$INSTDIR\compute\bin\illustash-research.exe"
  Delete "$INSTDIR\compute\bin\illustash-index.exe"
  Delete "$INSTDIR\compute\bin\illustash-provenance.exe"
  Delete "$INSTDIR\compute\bin\illustash-recognition.exe"
  ; Only a resources\ that is recognisably Illustash's own (base or older full install).
  IfFileExists "$INSTDIR\resources\web\static\index.html" 0 +3
  IfFileExists "$INSTDIR\resources\tags\tags_v3.csv" 0 +2
  RMDir /r "$INSTDIR\resources"
  ; The release package carries no models (#168): an older full install's would stay behind unused and
  ; unlisted. A full package copies its own again.
  RMDir /r "$INSTDIR\compute\resources\models"
!macroend
