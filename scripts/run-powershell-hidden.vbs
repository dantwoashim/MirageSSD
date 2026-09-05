Option Explicit

If WScript.Arguments.Count < 1 Then
    WScript.Quit 2
End If

Dim shell, powershell, command, index, value
Set shell = CreateObject("WScript.Shell")
powershell = shell.ExpandEnvironmentStrings("%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe")
command = Quote(powershell) & " -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File " & Quote(WScript.Arguments(0))

For index = 1 To WScript.Arguments.Count - 1
    value = WScript.Arguments(index)
    command = command & " " & Quote(value)
Next

WScript.Quit shell.Run(command, 0, True)

Function Quote(ByVal text)
    Quote = Chr(34) & Replace(text, Chr(34), Chr(34) & Chr(34)) & Chr(34)
End Function
