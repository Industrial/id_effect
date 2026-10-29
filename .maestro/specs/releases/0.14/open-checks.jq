.rows[] | select(.non_goal|not) | . as $row
| ((.checks|map(select(.non_goal|not))|map(.id)) - (.v2.met.id_effect["0.13.0"]//[])) as $open
| .checks[] | . as $c | select($open|index($c.id))
| "\($c.id) [\($row.v2.level.id_effect["0.13.0"][0:4])] \($c.text)"
