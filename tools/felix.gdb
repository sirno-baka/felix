set architecture i386
set disassembly-flavor intel
set pagination off
set confirm off

# Extra userspace symbols (ELF VMA already 0x400000).
# Uncomment / change the binary you are debugging:
# add-symbol-file /home/sirno/RustroverProjects/felix/target/x86_32-felix/debug/http-client
add-symbol-file /home/sirno/RustroverProjects/felix/target/x86_32-felix/debug/show
# add-symbol-file /home/sirno/RustroverProjects/felix/target/x86_32-felix/debug/shell
