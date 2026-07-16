#!/bin/sh
# Kompiliert die java.*-Stub-Klassenbibliothek (--patch-module-Trick, damit
# reservierte Pakete wie java.util kompilierbar sind).
set -e
dir="$(dirname "$0")"
find "$dir/java" -name '*.java' > "$dir/.sources"
javac --patch-module java.base="$dir" -d "$dir/out" @"$dir/.sources"
rm -f "$dir/.sources"
echo "Stub-Bibliothek in $dir/out/"
