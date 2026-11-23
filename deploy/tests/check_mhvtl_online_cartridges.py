#!/usr/bin/env python3
"""Exercise the shipped append-only media ledger and slot arbitration, no SCSI."""
import argparse
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument('--source', type=Path, required=True)
args = p.parse_args()
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    (root/'test.c').write_text(r'''
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
static int fail_sync;
static int test_sync(int fd) { if (fail_sync && --fail_sync == 0) {errno=EIO;return -1;} return fsync(fd); }
#define FM_FSYNC test_sync
#include "vaultcenter_media.h"
static struct fm_ledger ledger, restored;
int main(int argc, char **argv) {
 assert(argc==2);
 const char *op="00000000-0000-4000-8000-000000000001";
 const char *op2="00000000-0000-4000-8000-000000000002";
 struct fm_slot slots[4]={{2,1,0,""},{2,2,0,""},{2,3,0,"OLD001L8"},{4,10,1,"ACTIVE01"}};
 assert(fm_free_slot(slots,4)==1); /* reserve slot 1 for loaded drive */
 assert(fm_free_count(slots,4)==1);
 slots[3].origin=0;assert(fm_free_count(slots,4)==0);slots[3].origin=1;
 assert(fm_load(argv[1],20,&ledger)==0 && ledger.count==0);
 assert(fm_prepare(&ledger,op,"NEW001L8",slots,4)==1);
 assert(fm_prepare(&ledger,op,"ACTIVE01",slots,4)<0);
 assert(fm_add(&ledger,op,"NEW001L8")==0);
 /* A killed writer may leave a temporary file with the next process PID. */
 char orphan[4096];snprintf(orphan,sizeof(orphan),"%s.tmp.%ld",argv[1],(long)getpid());
 FILE *left=fopen(orphan,"w");assert(left);fputs("partial",left);fclose(left);
 snprintf(orphan,sizeof(orphan),"%s.tmp.%ld.0",argv[1],(long)getpid());
 left=fopen(orphan,"w");assert(left);fputs("partial",left);fclose(left);
 assert(fm_store(argv[1],20,&ledger)==0);
 assert(fm_load(argv[1],20,&restored)==0 && restored.count==1);
 assert(!strcmp(restored.ops[0].barcode,"NEW001L8"));
 /* Persisted intent can be replayed if the process died before publishing. */
 assert(fm_find(slots,4,restored.ops[0].barcode)==-1);
 strcpy(slots[1].barcode,"NEW001L8");
 assert(fm_prepare(&restored,op,"NEW001L8",slots,4)==FM_ALREADY);
 assert(fm_prepare(&restored,op2,"NEW001L8",slots,4)<0);
 assert(fm_prepare(&restored,op,"OTHER001",slots,4)<0);
 assert(fm_free_count(slots,4)==0);
 /* Retry after PBS has moved the new tape; location is not operation identity. */
 strcpy(slots[3].barcode,"NEW001L8");slots[3].origin=2;slots[1].barcode[0]=0;
 assert(fm_prepare(&restored,op,"NEW001L8",slots,4)==FM_ALREADY);
 assert(fm_free_slot(slots,4)==0);
 assert(!fm_valid_barcode("../TAPE"));assert(!fm_valid_operation("bad"));
 assert(fm_prepare(&restored,"bad","NEW002L8",slots,4)<0);
 assert(fm_load(argv[1],21,&restored)<0); /* foreign library */
 assert(fm_load(argv[1],20,&restored)==0);
 assert(fm_add(&restored,op2,"NEW002L8")==0);
 fail_sync=1; assert(fm_store(argv[1],20,&restored)<0);
 assert(fm_load(argv[1],20,&ledger)==0 && ledger.count==1); /* previous receipt intact */
 fail_sync=2; assert(fm_store(argv[1],20,&restored)<0);
 assert(fm_load(argv[1],20,&ledger)==0 && ledger.count==2); /* uncertain rename is recoverable */
 FILE *f=fopen(argv[1],"w");assert(f);fputs("VAULTCENTER_MEDIA1 20\ntruncated",f);fclose(f);
 assert(fm_load(argv[1],20,&ledger)<0);
 unlink(argv[1]);assert(symlink("missing",argv[1])==0);
 assert(fm_load(argv[1],20,&ledger)<0);
 return 0;
}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror',
                    '-I'+str(args.source.resolve()/'include'),str(root/'test.c'),'-o',str(root/'test')],check=True)
    subprocess.run([str(root/'test'),str(root/'ledger')],check=True,timeout=10)
print('PASS online cartridge core: slot reservations, duplicate operations, recovery, atomic persistence, corrupt and foreign journals')
# Compile the actual daemon adapter with an in-memory SMC inventory and a
# captured message transport. The cartridge and receipt I/O is real.
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    (root/'adapter.c').write_text(r'''
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/types.h>
#include <sys/stat.h>
#define CONF_FILE_SZ 4096
#define LIBCONTENTS 1
#define STATUS_Full 1
#define STATUS_Access 2
#define VTLCMD_Q 32768
#define QKEY 1
#define QPERM 0600
#define IPC_NOWAIT 1
struct m_info { char barcode[17]; };
struct s_info { struct s_info *next; unsigned element_type,slot_location,last_location,status; struct m_info *media; };
struct list { struct s_info *first; };
#define list_for_each_entry(p,h,m) for(p=(h)->first;p;p=p->next)
static struct {struct list slot_list;} smc_slots;
static struct {int online;} lunit;
static long my_id=20;
static char home_directory[4096],config_path[4096],answer[1025];
struct q_msg {long snd_id;char text[1025];};
struct q_entry {long rcv_id;struct q_msg msg;};
static int msgget(int a,int b){(void)a;(void)b;return 1;}
static int msgsnd(int a,const struct q_entry *r,size_t size,int flags){(void)a;(void)size;(void)flags;strcpy(answer,r->msg.text);return 0;}
static int slotOccupied(struct s_info *s){return !!(s->status&STATUS_Full);}
static struct m_info *lookup_barcode(void *lu,char *s){(void)lu;struct s_info *p;list_for_each_entry(p,&smc_slots.slot_list,siblings)if(p->media&&!strcmp(p->media->barcode,s))return p->media;return NULL;}
static struct m_info *add_barcode(void *lu,char *s){(void)lu;struct m_info *m=calloc(1,sizeof(*m));assert(m);strcpy(m->barcode,s);return m;}
#include "vaultcenter_library.h"
static void command(const char *text){struct q_msg m={.snd_id=40000};snprintf(m.text,sizeof(m.text),"%s",text);vaultcenter_media_command(&m);}
static void media(const char *barcode){assert(strlen(barcode)<=16);char path[sizeof(home_directory)+18];snprintf(path,sizeof(path),"%s/%.16s",home_directory,barcode);assert(mkdir(path,0700)==0);const char *names[]={"mam","mhvtl_data","data.0","indx.0","meta.0"};for(size_t i=0;i<5;i++){char file[8192];snprintf(file,sizeof(file),"%s/%s",path,names[i]);FILE *f=fopen(file,"w");assert(f);fclose(f);}}
int main(int argc,char **argv){assert(argc==2);snprintf(home_directory,sizeof(home_directory),"%s",argv[1]);snprintf(config_path,sizeof(config_path),"%s/library",argv[1]);
 struct m_info active={"ACTIVE01"};
 struct s_info drive={.element_type=4,.slot_location=10,.last_location=1,.status=STATUS_Full,.media=&active};
 struct s_info second={.element_type=2,.slot_location=2,.next=&drive};
 struct s_info first={.element_type=2,.slot_location=1,.next=&second};smc_slots.slot_list.first=&first;lunit.online=1;
 assert(vaultcenter_media_recover()==0);
 command("vaultcenter-media status 00000000-0000-4000-8000-000000000001");assert(strstr(answer,"\"free_slots\":1"));
 media("NEW001L8");
 command("vaultcenter-media insert 00000000-0000-4000-8000-000000000001 NEW001L8");assert(strstr(answer,"\"result\":\"inserted\""));
 assert(!first.media);assert(drive.media==&active);assert(!strcmp(second.media->barcode,"NEW001L8"));
 command("vaultcenter-media insert 00000000-0000-4000-8000-000000000001 NEW001L8");assert(strstr(answer,"already_inserted"));
 /* Restart with the unmodified base inventory: replay preserves the addition. */
 free(second.media);second.media=NULL;second.status=0;
 assert(vaultcenter_media_recover()==0);assert(!strcmp(second.media->barcode,"NEW001L8"));assert(drive.media==&active);
 command("vaultcenter-media insert 00000000-0000-4000-8000-000000000002 NEW001L8");assert(strstr(answer,"conflict_or_full"));
 command("vaultcenter-media status bad");assert(strstr(answer,"invalid_request"));
 vaultcenter_media_fault=1;command("vaultcenter-media status 00000000-0000-4000-8000-000000000001");assert(strstr(answer,"unavailable"));
 free(second.media);return 0;}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror',
                    '-I'+str(args.source.resolve()/'include'),str(root/'adapter.c'),'-o',str(root/'adapter')],check=True)
    subprocess.run([str(root/'adapter'),str(root)],check=True,timeout=10)
print('PASS daemon adapter: insertion, loaded media preservation, reboot replay, response identity and fault gate')
# Validate the actual CLI command validators rather than a parallel parser.
with tempfile.TemporaryDirectory() as tmp:
    root=Path(tmp)
    source=(args.source/'usr/cmd/vtlcmd.c').read_text()
    validators=source[source.index('int ishex('):source.index('int OpenExistingQueue(')]
    (root/'cli.c').write_text('''#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <stdlib.h>
#include <ctype.h>
#define TYPE_LIBRARY 1
#define TYPE_DRIVE 2
'''+validators+r'''
int main(void) {
 char *status[]={"vtlcmd","20","vaultcenter-media","status","00000000-0000-4000-8000-000000000001"};
 char *insert[]={"vtlcmd","20","vaultcenter-media","insert","00000000-0000-4000-8000-000000000001","NEW001L8"};
 assert(Check_Params(5,status)==NULL);
 assert(Check_Params(6,insert)==NULL);
 assert(Check_Params(4,status)!=NULL);
 assert(Check_DeviceCommand("vaultcenter-media status ",TYPE_LIBRARY)==NULL);
 assert(Check_DeviceCommand("vaultcenter-media status ",TYPE_DRIVE)!=NULL);
 return 0;
}
''')
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror','-Wno-unused-parameter',str(root/'cli.c'),'-o',str(root/'cli')],check=True)
    subprocess.run([str(root/'cli')],check=True,timeout=10)
print('PASS actual vtlcmd validators: status/insert accepted only for a library')
