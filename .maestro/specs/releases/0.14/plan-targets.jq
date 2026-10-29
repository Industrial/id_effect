# Writes planned 0.14.0 targets into a copy of scorecard.json: every SWEEP check met,
# every PROMOTE row at level production (cap 100), score = round(100 * met / scored checks).
def sweep: [
  "core.typed-effect#5","core.di#8","core.cause#7","core.interrupt#6","core.race#8",
  "conc.fibers#2","conc.coordination#8","conc.parallel#8","fault.supervision#8",
  "data.events#7","data.fp#7","data.fp#8","web.storage#6","sec.authz#7","sec.supply#7",
  "obs.console#7","msg.cron#6","msg.brokers#1","msg.brokers#2","eco.fit#6",
  "msg.email#6","msg.email#7","test.unit#4","test.determinism#8","test.chaos#5","test.chaos#6",
  "dx.scaffold#2","dx.cli#4","dx.cli#5","dx.docs#7","dx.diagnostics#8","dx.lint#6","dx.lint#8",
  "dx.repl#6","dx.deploy#6","eco.stability#4","eco.backing#8"];
def cap($l): {"absent":9,"labs":39,"documented":89,"production":100}[$l];
.rows |= map(
  if .non_goal then . else
    . as $r
    | ($r.checks | map(select(.non_goal|not)) | map(.id)) as $scored
    | (($r.v2.met.id_effect["0.13.0"] // []) + ($scored - ($r.v2.met.id_effect["0.13.0"] // []) | map(select(. as $c | sweep | index($c))))) as $met
    | ($r.v2.level.id_effect["0.13.0"]) as $lvl
    | (if $lvl == "documented" then "production" else $lvl end) as $lvl14
    | ([ (100 * ($met|length) / ($scored|length) + 0.5 | floor), cap($lvl14) ] | min) as $s
    | if $s > $r.id_effect["0.13.0"] then .targets["0.14.0"] = $s else . end
  end)
