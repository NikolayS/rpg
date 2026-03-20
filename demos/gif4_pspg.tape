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

Type "\version"
Enter
Sleep 2s

Enter
Sleep 300ms

# Built-in pager — show it works
Type "select id, name, email, plan, org_id, signed_up_at from customers limit 200;"
Enter
Sleep 3s

Sleep 1500ms
Down
Sleep 300ms
Down
Sleep 300ms
PageDown
Sleep 600ms
PageDown
Sleep 600ms

Type "q"
Sleep 800ms

Enter
Sleep 300ms

# Switch to pspg
Type "\set PAGER pspg"
Enter
Sleep 800ms

Enter
Sleep 300ms

# Same query — now routed through pspg
Type "select id, name, email, plan, org_id, signed_up_at from customers limit 200;"
Enter
Sleep 3s

# pspg is running — show horizontal scroll (Right arrow) and vertical scroll
Sleep 1500ms
Right
Sleep 300ms
Right
Sleep 300ms
Right
Sleep 300ms
Down
Sleep 300ms
Down
Sleep 300ms
PageDown
Sleep 600ms

Type "q"
Sleep 3s
