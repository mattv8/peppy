#!/bin/sh
set -eu

aar=${1:-build/gmlink.aar}
classes=build/gmlink-classes.jar
signature=build/gmlink-host.signature
unzip -p "$aar" classes.jar > "$classes"
javap_cmd=${JAVA_HOME:+$JAVA_HOME/bin/}javap
"$javap_cmd" -classpath "$classes" dev.peppy.gmlink.gmlink.Host > "$signature"
grep -Eq 'event\(java\.lang\.String, java\.lang\.String, long, long, long\)' "$signature" || {
	printf '%s\n' 'generated Host.event lacks generation/revision' >&2
	return 1 2>/dev/null || exit 1
}
grep -Eq 'showEmoji\(java\.lang\.String, long, long\)' "$signature" || {
	printf '%s\n' 'generated Host.showEmoji lacks generation/revision' >&2
	return 1 2>/dev/null || exit 1
}
