-- A function body with `;` inside a dollar quote, then a second statement.
CREATE FUNCTION widget_label(n TEXT) RETURNS TEXT AS $$
BEGIN
  RETURN 'widget: ' || n; -- a ';' inside the body
END;
$$ LANGUAGE plpgsql;

/* block comment; with a semicolon */
ALTER TABLE widgets ADD COLUMN label TEXT;
