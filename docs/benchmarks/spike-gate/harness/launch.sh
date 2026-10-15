#!/usr/bin/env bash
# Detach driver.sh from the tool session: launch.sh ARGS...
cd /data/Code/rust/id_effect
setsid nohup bash .tmp/spike-gate/driver.sh "$@" >>.tmp/spike-gate/driver.log 2>&1 </dev/null &
echo "launched pid $!"
