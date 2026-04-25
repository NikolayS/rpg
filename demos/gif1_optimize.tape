Output demos/gif1_optimize.gif
Set FontSize 16
Set Width 1200
Set Height 700
Set Theme "Dracula"
Set Shell "zsh"
Set TypingSpeed 40ms

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

Type "select * from orders where status = 'pending' order by created_at desc limit 100;"
Enter
Sleep 4s

Down
Sleep 400ms
Down
Sleep 400ms
PageDown
Sleep 800ms
PageDown
Sleep 800ms

Type "q"
Sleep 1s

Enter
Sleep 300ms

Type "/explain"
Enter
Sleep 10s

Enter
Sleep 300ms

Type "/optimize"
Enter
Sleep 10s

Enter
Sleep 300ms

Type "create index concurrently on orders (status, created_at desc);"
Enter
Sleep 5s

Enter
Sleep 300ms

Type "select * from orders where status = 'pending' order by created_at desc limit 100;"
Enter
Sleep 4s

Down
Sleep 400ms
Down
Sleep 400ms

Type "q"
Sleep 3s
