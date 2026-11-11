# AEAT Veri*factu schemas

Unmodified copies of the official WSDL and XSD files of the Veri*factu web services, used to validate the XML this project produces and to build the AEAT mock.

- Source: `https://www2.agenciatributaria.gob.es/static_files/common/internet/dep/aplicaciones/es/aeat/tike/cont/ws/`
- `xmldsig-core-schema.xsd` comes from `https://www.w3.org/TR/xmldsig-core/` and is imported by `SuministroInformacion.xsd`.
- Downloaded on 2026-11-11. Checksums are in `SHA256SUMS`.

Do not edit these files. To update them, download them again, refresh `SHA256SUMS` and run the tests. `catalog.xml` maps the remote XML Signature schema to the local copy:

```bash
XML_CATALOG_FILES=aeat/schemas/catalog.xml xmllint --nonet --noout \
  --schema aeat/schemas/SuministroLR.xsd request.xml
```
