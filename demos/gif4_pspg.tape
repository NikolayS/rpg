Output demos/gif4_pspg.gif
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set Shell "zsh"
Set TypingSpeed 30ms

Hide
Type "export PATH=/Users/nik/github/project-alpha/target/debug:$PATH"
Enter
Sleep 500ms
Type "clear"
Enter
Sleep 500ms
Show

Type "rpg -d demo_saas"
Sleep 500ms
Enter
Sleep 2s

# Set pspg immediately — no native pager demo
# --menu-always shows the F9 menu bar on every page
Type "\set PAGER 'pspg --style=22 --menu-always'"
Enter
Sleep 1s

# Run a wide query — menu bar is always visible (F9 effect)
Type "select * from customers limit 50;"
Enter
Sleep 3s

# Show horizontal scroll (killer feature)
Right
Sleep 400ms
Right
Sleep 400ms
Right
Sleep 400ms

# Show column cursor (Alt+v)
Alt+v
Sleep 1500ms

# Exit pspg
Type "q"
Sleep 1s

# Demo theme switching — switch to a lighter theme (style 3)
Type "\set PAGER 'pspg --style=3 --menu-always'"
Enter
Sleep 800ms

Type "select * from customers limit 50;"
Enter
Sleep 3s

# Exit
Type "q"
Sleep 2s
