### Das Backoffice der Pipeline auf Deutsch: die Prüfwarteschlange, die Texte der
### Administration und die Bezeichnungen der Entitäten. Jede Meldung aus
### `locales/en/backoffice.ftl` muss hier stehen (der Katalog prüft es beim Start).

## Die Prüfwarteschlange (src/ui/review.rs)

backoffice = Backoffice
    .rerun-extraction = Extraktion erneut ausführen
nav-queue = Prüfwarteschlange
nav-admin = Administration
nav-language = Sprache
nav-switch = Wechseln
locale-en = English
locale-de = Deutsch
review-title = Prüfwarteschlange
review-count = { $count ->
    [one] Ein Dokument wartet auf Prüfung.
   *[other] { $count } Dokumente warten auf Prüfung.
}
review-empty = Kein Dokument wartet auf Prüfung.
review-column-title = Titel
review-column-source = Quelle
review-column-labels = Labels
review-column-duplicate = Beinahe-Duplikat
review-open = Prüfen
review-select = Wählen Sie ein Dokument zur Prüfung aus.
review-body = Text
review-extraction = Extraktion
review-no-extraction = Noch keine Extraktion.
review-no-labels = Keine Labels.
review-duplicate = Beinahe ein Duplikat von Dokument { $id }.
review-field-title = Titel
review-accept = Annehmen und veröffentlichen
review-published = „{ $title }“ ist veröffentlicht.
review-invalid = Bitte korrigieren Sie die Fehler unten.
review-conflict = Jemand anderes hat dieses Dokument geändert. Laden Sie es neu und versuchen Sie es noch einmal.
review-not-in-review = Dieses Dokument wartet nicht mehr auf Prüfung.
review-not-found = Dieses Dokument gibt es nicht.
review-error = Etwas ist schiefgegangen. Versuchen Sie es später noch einmal.
review-bad-locale = Diese Sprache ist nicht verfügbar.
status-ingested = eingegangen
status-extracted = extrahiert
status-classified = klassifiziert
status-review = in Prüfung
status-published = veröffentlicht
status-failed = fehlgeschlagen

## Die Administration (Texte von id_effect_admin und Bezeichnungen der Entitäten)

admin = Administration
    .search = Suchen
    .apply = Anwenden
    .any = Alle
    .yes = Ja
    .no = Nein
    .results = { $count ->
        [one] Ein Treffer
       *[other] { $count } Treffer
    }
    .empty = Keine Einträge gefunden.
    .previous = Zurück
    .next = Weiter
    .back = Zurück zur Liste
    .nothing = Nichts zu verwalten.
    .forbidden = Das dürfen Sie nicht.
    .not-found = Diesen Eintrag gibt es nicht.
    .bad-request = Diese Anfrage ist ungültig.
    .error = Etwas ist schiefgegangen.
    .new = Neu
    .create = Anlegen
    .edit = Bearbeiten
    .save = Speichern
    .cancel = Abbrechen
    .delete = Löschen
    .delete-confirm = Diesen Eintrag löschen? Das lässt sich nicht rückgängig machen.
    .invalid = Bitte korrigieren Sie die Fehler unten.
    .conflict = Jemand anderes hat diesen Eintrag geändert. Laden Sie ihn neu und versuchen Sie es noch einmal.
    .duplicate = Einen Eintrag mit diesem Schlüssel gibt es schon.
    .csrf = Dieses Formular ist abgelaufen. Laden Sie die Seite neu und versuchen Sie es noch einmal.
    .too-large = Dieses Formular ist zu groß.
    .too-many = Zu viele Einträge ausgewählt.
    .select = Auswählen
    .selected = Mit den ausgewählten Einträgen:
    .run = Ausführen
    .find = Finden
    .add = Hinzufügen
    .remove = Entfernen
    .none = Keine.
pipeline = Pipeline
    .source = Quellen
    .document = Dokumente
    .label = Labels
    .extraction = Extraktionen
source = Quelle
    .id = ID
    .name = Name
    .url = URL
    .trust = Vertrauen
document = Dokument
    .id = ID
    .source_id = Quelle
    .title = Titel
    .body = Text
    .status = Status
    .duplicate_of = Beinahe-Duplikat von
    .version = Version
label = Label
    .id = ID
    .name = Name
    .description = Beschreibung
