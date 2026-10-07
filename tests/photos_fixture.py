"""Rows for a synthetic Photos library database (former tests/test_sync.py helper)."""
import sqlite3


def photo(self, uuid, app, trashed=0):
    with sqlite3.connect(self.library / 'database' / 'Photos.sqlite') as p:
        pk = p.execute('INSERT INTO ZADDITIONALASSETATTRIBUTES(ZIMPORTEDBYBUNDLEIDENTIFIER,ZORIGINALSTABLEHASH,ZORIGINALFILESIZE) '
                       'VALUES (?,?,?)', (app, 'h-' + uuid, 100)).lastrowid
        p.execute('INSERT INTO ZASSET(ZUUID,ZKIND,ZTRASHEDSTATE,ZWIDTH,ZHEIGHT,ZADDEDDATE,ZHIDDEN,ZDIRECTORY,ZFILENAME,'
                  'ZADDITIONALATTRIBUTES) VALUES (?,0,?,120,150,780000000,0,?,?,?)', (uuid, trashed, uuid[0], uuid + '.png', pk))
