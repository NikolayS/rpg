Output demos/gif4_pspg.gif
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set Shell "zsh"
Set TypingSpeed 30ms

Hide
Type "export PATH=/Users/nik/github/rpg/target/debug:$PATH"
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

Type "\version"
Enter
Sleep 2s

Enter
Sleep 300ms

Type "select * from customers limit 50;"
Enter
Sleep 3s

Down
Sleep 400ms
Down
Sleep 400ms
PageDown
Sleep 800ms

Type "q"
Sleep 1s

Enter
Sleep 300ms

# Switch to pspg with Relational pipes theme (style 22 — clean dark theme)
Type "\set PAGER 'pspg --style=22'"
Enter
Sleep 1s

Type "select * from customers limit 50;"
Enter
Sleep 3s

# Alt+n — toggle line numbers on (visible change, like F6)
Alt+n
Sleep 1500ms

# Alt+n — toggle line numbers off
Alt+n
Sleep 800ms

# Alt+v — toggle vertical column cursor (like F5)
Alt+v
Sleep 1500ms

# Right arrow — horizontal scroll (pspg's killer feature)
Right
Sleep 500ms
Right
Sleep 500ms
Right
Sleep 500ms
Right
Sleep 500ms

Type "q"
Sleep 2s
