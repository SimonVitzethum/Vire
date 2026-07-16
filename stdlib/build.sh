#!/bin/sh
# Kompiliert die java.util-Stub-Klassenbibliothek. Der --patch-module-Trick
# erlaubt es, Klassen im reservierten java.util-Paket zu kompilieren; die
# entstehenden .class-Dateien gibt man fastjavac zusammen mit dem
# Nutzercode. Nutzercode wird ganz normal gegen das echte JDK kompiliert.
set -e
dir="$(dirname "$0")"
javac --patch-module java.base="$dir" -d "$dir/out" "$dir"/java/util/*.java
echo "Stub-Bibliothek in $dir/out/"
