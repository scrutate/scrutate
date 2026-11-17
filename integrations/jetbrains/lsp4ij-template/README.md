# Scrutate

`scrutate lsp` answers Go to Declaration, Find Usages, Go to Symbol and the Structure view from the
Scrutate index of the project. It offers no completion, hover or diagnostics.

1. Put `scrutate` on the IDE's `PATH`, or replace `scrutate` in the command with the full path to
   the binary. On Windows always use the full path (`C:\Users\me\.cargo\bin\scrutate.exe lsp`):
   Windows looks for a bare `scrutate.exe` in the IDE's current folder too.
2. Keep the file name patterns of the languages you want answered, or add others. The server
   ignores the language id.
3. Click **OK** and open a project file. The first start builds `.scrutate/` in the project.
