items = { $count ->
    [one] one
   *[other] other
}
files = { $count ->
    [one] { $count } Datei
   *[other] { $count } Dateien
}
