/* Looker's build of libzvbi leaves out packet-830.c and pdc.c, the two GPL-2 files of the library. packet.c only
 * calls these for VBI_EVENT_LOCAL_TIME and VBI_EVENT_PROG_ID handlers (FFmpeg registers VBI_EVENT_TTX_PAGE only),
 * so here they report "nothing decoded". Written for Looker's port, under zvbi's LGPL-2.0+. */
#include <time.h>
#include "misc.h"
#include "packet-830.h"

vbi_bool
vbi_decode_teletext_8301_cni(unsigned int *cni, const uint8_t buffer[42])
{
	(void) cni; (void) buffer;
	return FALSE;
}

vbi_bool
vbi_decode_teletext_8301_local_time(time_t *utc_time, int *seconds_east, const uint8_t buffer[42])
{
	(void) utc_time; (void) seconds_east; (void) buffer;
	return FALSE;
}

vbi_bool
vbi_decode_teletext_8302_cni(unsigned int *cni, const uint8_t buffer[42])
{
	(void) cni; (void) buffer;
	return FALSE;
}

vbi_bool
vbi_decode_teletext_8302_pdc(vbi_program_id *pid, const uint8_t buffer[42])
{
	(void) pid; (void) buffer;
	return FALSE;
}
