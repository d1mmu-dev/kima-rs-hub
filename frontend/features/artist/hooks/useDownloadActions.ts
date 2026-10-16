import { useCallback } from 'react';
import { api } from '@/lib/api';
import { notify } from "@/lib/notify";
import { isMbid } from "@/lib/mbid";
import { useDownloadContext } from '@/lib/download-context';
import { Artist, Album } from '../types';

export function useDownloadActions() {
  const { addPendingDownload, isPendingByMbid } = useDownloadContext();

  const downloadArtist = useCallback(
    async (artist: Artist | null) => {
      if (!artist) return;

      if (!isMbid(artist.mbid)) {
        notify.error("Can't download this artist", { message: 'They have no MusicBrainz ID.' });
        return;
      }

      if (isPendingByMbid(artist.mbid)) return;

      try {
        addPendingDownload('artist', artist.name, artist.mbid);
        await api.downloadArtist(artist.name, artist.mbid);
      } catch (error: unknown) {
        console.error('Failed to download artist:', error);
        notify.error('Failed to download artist', { message: error instanceof Error ? error.message : undefined });
      }
    },
    [addPendingDownload, isPendingByMbid]
  );

  const downloadAlbum = useCallback(
    async (album: Album, artistName: string) => {
      const mbid = [album.rgMbid, album.mbid].find(isMbid);

      if (!mbid) {
        notify.error("Can't download this album", { message: 'It has no MusicBrainz ID.' });
        return;
      }

      if (isPendingByMbid(mbid)) return;

      try {
        addPendingDownload('album', `${artistName} - ${album.title}`, mbid);
        await api.downloadAlbum(artistName, album.title, mbid);
      } catch (error: unknown) {
        console.error('Failed to download album:', error);
        notify.error('Failed to download album', { message: error instanceof Error ? error.message : undefined });
      }
    },
    [addPendingDownload, isPendingByMbid]
  );

  return {
    downloadArtist,
    downloadAlbum,
  };
}
