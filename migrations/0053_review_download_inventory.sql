-- Review U06. Versions 0045-0049 are left for ongoing completion/access work.
CREATE INDEX downloads_active ON downloads(device_id,principal_id,id) WHERE state='ready';
