<p align="center">
  <a href="https://github.com/New-Elysium/libsql-python">
    <img alt="Turso + Python" src="https://github.com/tursodatabase/libsql-python/assets/950181/3748f2b3-872e-4cdd-bbe3-78491fd81dfa" width="1000">
    <h3 align="center">Turso + Python</h3>
  </a>
</p>

<p align="center">
  SQLite for Production. Powered by <a href="https://turso.tech/libsql">libSQL</a>.
</p>

<p align="center">
  <a href="https://turso.tech"><strong>Turso</strong></a> ·
  <a href="https://docs.turso.tech/quickstart"><strong>Quickstart</strong></a> ·
  <a href="/examples"><strong>Examples</strong></a> ·
  <a href="https://docs.turso.tech"><strong>Docs</strong></a> ·
  <a href="https://github.com/New-Elysium/libsql-python"><strong>GitHub</strong></a> ·
  <a href="https://discord.gg/turso"><strong>Discord</strong></a> ·
  <a href="https://blog.turso.tech/"><strong>Blog &amp; Tutorials</strong></a>
</p>

<p align="center">
  <a href="https://pypi.org/project/libsql-py">
    <img src="https://badge.fury.io/py/libsql-py.svg" alt="PyPI" title="PyPI" />
  </a>
</p>

---

## Installation

The package is published to PyPI as **[`libsql-py`](https://pypi.org/project/libsql-py)**:

```sh
pip install libsql-py
```

The importable module stays **`libsql`**, so it is a drop-in replacement — any code, tutorial, or documentation written against `import libsql` keeps working unchanged:

```python
import libsql

conn = libsql.connect("local.db")
conn.execute("CREATE TABLE users (id INTEGER, email TEXT)")
conn.execute("INSERT INTO users VALUES (1, 'alice@example.com')")
print(conn.execute("SELECT * FROM users").fetchall())
```

> [!NOTE]
> If the original [`libsql`](https://pypi.org/project/libsql) distribution is installed, uninstall it first (`pip uninstall libsql`). Both distributions provide the same `libsql` module, and having both installed would leave the import target ambiguous.

## Source Code

Upstream repository: [New-Elysium/libsql-python](https://github.com/New-Elysium/libsql-python)

## Documentation

1. [Turso Quickstart](https://docs.turso.tech/quickstart) &mdash; Learn how create and connect your first database.
2. [SDK Quickstart](https://docs.turso.tech/sdk/python/quickstart) &mdash; Learn how to install and execute queries using the libSQL client.
3. [SDK Reference](https://docs.turso.tech/sdk/python/reference) &mdash; Dive deeper with the libSQL SDK reference and examples.

### What is Turso?

[Turso](https://turso.tech) is a SQLite-compatible database built on [libSQL](https://docs.turso.tech/libsql), the Open Contribution fork of SQLite. It enables scaling to hundreds of thousands of databases per organization and supports replication to any location, including your own servers, for microsecond-latency access.

Learn more about what you can do with Turso:

- [Embedded Replicas](https://docs.turso.tech/features/embedded-replicas)
- [Platform API](https://docs.turso.tech/features/platform-api)
- [Branching](https://docs.turso.tech/features/branching)
- [Point-in-Time Recovery](https://docs.turso.tech/features/point-in-time-recovery)
