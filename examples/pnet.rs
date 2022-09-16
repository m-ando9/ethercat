use ethercat_master::hal::*;
use ethercat_master::interface::*;
use ethercat_master::network::AlState;
use ethercat_master::network::PdoEntry;
use ethercat_master::network::PdoMapping;
use ethercat_master::network::SlaveConfig;
use ethercat_master::register::sii::ProductCode;
use ethercat_master::EtherCatMaster;
use ethercat_master::task::EtherCatSystemTime;
use pnet_datalink::{self, Channel::Ethernet, DataLinkReceiver, DataLinkSender, NetworkInterface};
use std::env;

struct PnetDevice {
    tx_buf: [u8; 1500],
    tx: Box<dyn DataLinkSender>,
    rx: Box<dyn DataLinkReceiver>,
}

impl PnetDevice {
    fn open(network_interface_name: &str) -> Self {
        let interface_names_match = |iface: &NetworkInterface| iface.name == network_interface_name;
        let interfaces = pnet_datalink::interfaces();
        let interface = interfaces
            .into_iter()
            .find(interface_names_match)
            .expect("interface not found");
        let (tx, rx) = match pnet_datalink::channel(&interface, Default::default()) {
            Ok(Ethernet(tx, rx)) => (tx, rx),
            Ok(_) => panic!("unhandled interface"),
            Err(_e) => panic!("unenable to create channel"),
        };
        Self {
            tx_buf: [0; 1500],
            tx,
            rx,
        }
    }
}

impl<'a> RawEthernetDevice<'a> for PnetDevice {
    type TxToken = PnetTxToken<'a>;
    type RxToken = PnetRxToken<'a>;
    fn transmit(&'a mut self) -> Option<Self::TxToken> {
        Some(PnetTxToken(&mut self.tx, &mut self.tx_buf))
    }

    fn receive(&'a mut self) -> Option<Self::RxToken> {
        Some(PnetRxToken(&mut self.rx))
    }
}

struct PnetTxToken<'a>(&'a mut Box<dyn DataLinkSender + 'static>, &'a mut [u8]);
impl<'a> TxToken for PnetTxToken<'a> {
    fn consume<F>(self, len: usize, f: F) -> Result<(), ()>
    where
        F: FnOnce(&mut [u8]) -> Result<(), ()>,
    {
        let b = f(&mut self.1[..len]);
        if let Some(r) = self.0.send_to(&self.1[..len], None) {
            match r {
                Ok(_) => b,
                Err(_) => Err(()),
            }
        } else {
            Err(())
        }
    }
}
struct PnetRxToken<'a>(&'a mut Box<dyn DataLinkReceiver>);
impl<'a> RxToken for PnetRxToken<'a> {
    fn consume<F>(self, f: F) -> Result<(), ()>
    where
        F: FnOnce(&[u8]) -> Result<(), ()>,
    {
        self.0.next().ok().map(f).unwrap_or(Err(()))
    }
}

fn main() {
    env::set_var("RUST_LOG", "info");
    env_logger::init();
    let args: Vec<String> = env::args().collect();

    if let Some(name) = args.get(1) {
        read_eeprom_test(name);
        sdo_test(name);
    } else {
        println!("Specify the name of network interface as an argument from the following.");
        for (i, interface) in pnet_datalink::interfaces().iter().enumerate() {
            println!("{}:", i);
            println!("    Description: {}", interface.description);
            println!("    Name: {}", interface.name);
        }
    }
}

fn read_eeprom_test(interf_name: &str) {
    dbg!("prepare resources");
    let device = PnetDevice::open(interf_name);
    let mut buf = vec![0; 1500];
    let iface = CommandInterface::new(device, &mut buf);

    let mut socket_buf = vec![0; 256];
    let sockets = [
        SocketOption::default(), // al state
    ];
    let mut sif = SocketsInterface::new(iface, sockets);
    let handle = sif.add_socket(CommandSocket::new(&mut socket_buf)).unwrap();
    let (data, size) = sif
        .read_sii(
            &handle,
            SlaveAddress::SlavePosition(1),
            ProductCode::ADDRESS,
        )
        .unwrap();
    dbg!(data.data(size));
}

fn sdo_test(interf_name: &str) {
    dbg!("prepare resources");
    let device = PnetDevice::open(interf_name);
    let mut buf = vec![0; 1500];
    let iface = CommandInterface::new(device, &mut buf);

    let mut slaves: [_; 10] = Default::default();
    let mut pdu_buffer = vec![0; 1500];
    let mut master = EtherCatMaster::new(&mut slaves, &mut pdu_buffer, iface);
    master.initilize_slaves().unwrap();
    let num_slaves = master.network().num_slaves();
    master
        .change_al_state(TargetSlave::All(num_slaves), AlState::PreOperational)
        .unwrap();
    let data = master
        .read_sdo(SlaveAddress::SlavePosition(0), 0x2005, 0x01)
        .unwrap();
    dbg!(data);

    let data2 = [data[0] + 1, data[1]];
    master
        .write_sdo(SlaveAddress::SlavePosition(0), 0x2005, 0x01, &data2)
        .unwrap();

    let data = master
        .read_sdo(SlaveAddress::SlavePosition(0), 0x2005, 0x01)
        .unwrap();
    dbg!(data);

    let data2 = [data[0] - 1, data[1]];
    master
        .write_sdo(SlaveAddress::SlavePosition(0), 0x2005, 0x01, &data2)
        .unwrap();
}

fn pdo_test(interf_name: &str) {
    dbg!("prepare resources");
    let device = PnetDevice::open(interf_name);
    let mut buf = vec![0; 1500];
    let iface = CommandInterface::new(device, &mut buf);

    let tx_pdo_map = PdoMapping {
        is_fixed: false,
        entries: &mut [
            PdoEntry::new(0x603F, 0x00, 16), // error code
            PdoEntry::new(0x6041, 0x00, 16), // status word
            PdoEntry::new(0x6064, 0x00, 32), // actual position
            PdoEntry::new(0x6077, 0x00, 16), // actual torque
            PdoEntry::new(0x60F4, 0x00, 32), // position error
        ],
    };
    let mut tx_maps = [tx_pdo_map];

    let rx_pdo_map = PdoMapping {
        is_fixed: false,
        entries: &mut [
            PdoEntry::new(0x6040, 0x00, 16), // control word
            PdoEntry::new(0x607A, 0x00, 32), // target position
        ],
    };
    let mut rx_maps = [rx_pdo_map];

    let mut slaves: Box<[(_, SlaveConfig); 10]> = Box::new(Default::default());
    slaves[0].1.set_tx_pdo_mappings(&mut tx_maps);
    slaves[0].1.set_rx_pdo_mappings(&mut rx_maps);
    let mut pdu_buffer = vec![0; 1500];
    let mut pdo_buffer = vec![0; 1500];
    let mut master = EtherCatMaster::new(slaves.as_mut(), &mut pdu_buffer, iface);
    master
        .configure_pdo_settings_and_change_to_safe_operational_state(&mut pdo_buffer)
        .unwrap();
    let num_slaves = master.network().num_slaves();
    master.change_al_state(TargetSlave::All(num_slaves), AlState::Operational).unwrap();
    let mut time = 0;
    let mut pre_cycle_count=0;
    for i in 0..1000{
        loop{
            time+=1;
            let cycle_count = master.process_one_cycle(EtherCatSystemTime(time)).unwrap();
            if pre_cycle_count < cycle_count{
                pre_cycle_count = cycle_count;
                break;
            }
        }
    }

}
