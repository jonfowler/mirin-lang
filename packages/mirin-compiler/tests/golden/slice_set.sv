module concat (
    input  logic [7:0] lo_byte,
    input  logic [7:0] hi_byte,
    output logic [15:0] result
);
    logic [15:0] word;
    assign word[0 +: 8] = lo_byte;
    assign word[8 +: 8] = hi_byte;
    assign result = word;
endmodule
