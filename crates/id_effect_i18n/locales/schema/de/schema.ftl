### Validierungsmeldungen der id_effect-`Schema`-Dekodierung (id_effect_i18n, Feature `schema`).
### Eine Meldung je `SchemaErrorCode`; Argumente wie in `en/schema.ftl`.

schema-missing-field = Dieses Feld ist erforderlich.
schema-required = Ein Wert ist erforderlich.
schema-expected-object = Erwartet wird ein Objekt.
schema-expected-integer = Erwartet wird eine ganze Zahl.
schema-expected-number = Erwartet wird eine Zahl.
schema-expected-string = Erwartet wird ein Text.
schema-expected-boolean = Erwartet wird wahr oder falsch.
schema-expected-array = Erwartet wird eine Liste.
schema-expected-array-length = { $length ->
    [one] Erwartet wird eine Liste mit { $length } Eintrag.
   *[other] Erwartet wird eine Liste mit { $length } Einträgen.
}
schema-expected-literal = Erwartet wird { $value }.
schema-not-empty = Darf nicht leer sein.
schema-min-length = Muss mindestens { $min } Zeichen lang sein.
schema-max-length = Darf höchstens { $max } Zeichen lang sein.
schema-pattern = Muss dem Muster { $pattern } entsprechen.
schema-email = Muss eine gültige E-Mail-Adresse sein.
schema-uuid = Muss eine gültige UUID sein.
schema-url = Muss eine gültige URL sein.
schema-one-of = Muss einer der folgenden Werte sein: { $values }.
schema-minimum = Muss mindestens { $min } sein.
schema-maximum = Darf höchstens { $max } sein.
schema-exclusive-minimum = Muss größer als { $min } sein.
schema-exclusive-maximum = Muss kleiner als { $max } sein.
schema-min-items = { $min ->
    [one] Muss mindestens { $min } Eintrag enthalten.
   *[other] Muss mindestens { $min } Einträge enthalten.
}
schema-max-items = { $max ->
    [one] Darf höchstens { $max } Eintrag enthalten.
   *[other] Darf höchstens { $max } Einträge enthalten.
}
schema-no-match = Entspricht keiner der erlaubten Formen.
