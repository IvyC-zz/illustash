; Windows installer hooks shared by the base and full installers (#173).
; The installer has English and Simplified Chinese pages and no language dialog: NSIS picks the
; one matching the Windows display language (any Chinese variant -> Simplified Chinese, anything
; else -> English, the first listed). The Tauri uninstaller reads the language from
; "Installer Language" and shows an English "Please select a language" dialog when the value is
; missing, but the Tauri installer only writes it when its own language dialog is shown, so save
; the language this install used.
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "${MANUPRODUCTKEY}" "Installer Language" $LANGUAGE
!macroend
