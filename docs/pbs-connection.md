# Connecting a VaultCenter library to PBS

In **Virtual libraries → Details → iSCSI connection**, the console generates a procedure specific to the selected VTL. Run the commands on **the PBS of that VTL**, in a **root** shell (the root account works without installing sudo).

1. Check in **Backup client** that the VTL is of type PBS (read-only field, managed by the appliance configuration), configure cartridge storage and prepare/apply the hardware. The identities come from the validated hardware manifest; no command is offered while it is missing or invalid.
2. Enter the iSCSI IP address or DNS name of the appliance **reachable from PBS**, then its port (3260 by default). The web port, for example 8080, is independent. For IPv6, enter the address without brackets and the port separately.
3. Click **Enable iSCSI access** in VaultCenter. Activation may remain pending until the drive is registered on the PBS side.
4. On PBS, check that `open-iscsi`, `sg3-utils` and Python 3 are present. Copy and run, one after the other, **Connect the iSCSI target**, **Register the changer and drive**, then **Check inventory**.
5. Resume activation in VaultCenter if it is pending. The cartridges then appear in the inventory of the PBS changer; their labelling and the pools/jobs remain managed in PBS.

The commands embed the IQN, the serial numbers and the PBS names of this VTL only. They reuse an identical configuration and refuse name or identity collisions. They do not disconnect any session and do not start any global discovery. A session that is already connected keeps its negotiated parameters; the saved optimised profile will be used at the next connection, between two jobs. The procedure does not force a reconnection during a backup.

The iSCSI profile uses ImmediateData=Yes, InitialR2T=No and bursts/segments of 262144 bytes. It does not change the global iscsid values or other targets such as StarWind. Historical IQNs and technical paths containing `vaultcenter` remain valid after the rename to VaultCenter.

Over HTTP, some browsers block automatic copying. In that case, the console tells you to select and copy the text manually. The “Copied” message appears only if the clipboard accepted the command.

The `GET/PUT /v1/vtls/<UUID>/pbs-connection` APIs are restricted to administrators. The PUT accepts `{revision, host, port}` and requires the CSRF token. The responses contain no SSH key, no password and no configuration of another VTL.

Reading the serial number uses `sg_inq --page=0x80 --export`: the default text format may be hexadecimal depending on the sg3-utils version. Initial creation and collisions are covered by tests with simulated commands.

For a Veeam VTL, see [veeam.md](veeam.md).
