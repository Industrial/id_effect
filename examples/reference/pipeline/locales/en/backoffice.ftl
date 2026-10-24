### The Pipeline's back office in English (the default locale): the review queue, the admin's
### page text and entity labels. `locales/de/backoffice.ftl` must define every message here
### (the catalog is built with `require_complete`). Schema validation messages come from
### id_effect_i18n (`schema-*`).

## The review queue (src/ui/review.rs)

backoffice = Back office
    .rerun-extraction = Re-run extraction
nav-queue = Review queue
nav-admin = Administration
nav-language = Language
nav-switch = Switch
locale-en = English
locale-de = Deutsch
review-title = Review queue
review-count = { $count ->
    [one] One document waits for review.
   *[other] { $count } documents wait for review.
}
review-empty = No document is waiting for review.
review-column-title = Title
review-column-source = Source
review-column-labels = Labels
review-column-duplicate = Near-duplicate
review-open = Review
review-select = Select a document to review it.
review-body = Text
review-extraction = Extraction
review-no-extraction = No extraction yet.
review-no-labels = No labels.
review-duplicate = Nearly duplicates document { $id }.
review-field-title = Title
review-accept = Accept and publish
review-published = “{ $title }” is published.
review-invalid = Please correct the errors below.
review-conflict = Someone else changed this document. Reload it and try again.
review-not-in-review = This document is no longer waiting for review.
review-not-found = This document does not exist.
review-error = Something went wrong. Try again later.
review-bad-locale = This language is not available.
status-ingested = ingested
status-extracted = extracted
status-classified = classified
status-review = in review
status-published = published
status-failed = failed

## The admin (id_effect_admin's page text and the entities' labels)

admin = Administration
    .search = Search
    .apply = Apply
    .any = Any
    .yes = Yes
    .no = No
    .results = { $count ->
        [one] One result
       *[other] { $count } results
    }
    .empty = No records match.
    .previous = Previous
    .next = Next
    .back = Back to the list
    .nothing = Nothing to administer.
    .forbidden = You may not do this.
    .not-found = This record does not exist.
    .bad-request = This request is not valid.
    .error = Something went wrong.
    .new = New
    .create = Create
    .edit = Edit
    .save = Save
    .cancel = Cancel
    .delete = Delete
    .delete-confirm = Delete this record? This cannot be undone.
    .invalid = Please correct the errors below.
    .conflict = Someone else changed this record. Reload it and try again.
    .duplicate = A record with this key already exists.
    .csrf = This form has expired. Reload the page and try again.
    .too-large = This form is too large.
    .too-many = Too many records are selected.
    .select = Select
    .selected = With the selected records:
    .run = Run
    .find = Find
    .add = Add
    .remove = Remove
    .none = None.
pipeline = Pipeline
    .source = Sources
    .document = Documents
    .label = Labels
    .extraction = Extractions
source = Source
    .id = ID
    .name = Name
    .url = URL
    .trust = Trust
document = Document
    .id = ID
    .source_id = Source
    .title = Title
    .body = Text
    .status = Status
    .duplicate_of = Near-duplicate of
    .version = Version
label = Label
    .id = ID
    .name = Name
    .description = Description
