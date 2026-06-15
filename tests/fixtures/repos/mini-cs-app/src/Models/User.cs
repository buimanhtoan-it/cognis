namespace MiniApp.Models
{
    public enum Role
    {
        Guest,
        Member,
        Admin
    }

    public class User
    {
        public string Name { get; set; }

        public Role Role { get; set; }
    }
}
