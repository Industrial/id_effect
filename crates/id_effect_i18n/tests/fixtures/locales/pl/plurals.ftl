items = { $count ->
    [one] one
    [few] few
    [many] many
   *[other] other
}
files = { $count ->
    [one] { $count } plik
    [few] { $count } pliki
   *[many] { $count } plików
}
