# The CLDR plural category Fluent selects, by name.
items = { $count ->
    [one] one
   *[other] other
}
files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
